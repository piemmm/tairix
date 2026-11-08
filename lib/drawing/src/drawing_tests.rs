//! Unit tests for drawings.

#![allow(
    clippy::float_cmp,
    reason = "the bounds compared are sums of small integers, exact by construction"
)]

use alloc::sync::Arc;
use alloc::vec;

use super::{Drawing, Grid, Page, Unit};
use crate::children::Children;
use crate::id::Ids;
use crate::node::{Kind, Node};
use crate::shape::{RectShape, Shape};
use crate::style::Style;
use crate::testing::slice;
use crate::DrawingError;
use crate::SharedSlice;

fn page() -> Page {
    Page {
        width: 800.0,
        height: 600.0,
        unit: Unit::Px,
        background: None,
    }
}

fn rect(ids: &mut Ids, x: f64) -> Arc<Node> {
    Arc::new(
        Node::new(
            ids.issue(),
            Kind::Shape {
                shape: Shape::Rect(RectShape {
                    x,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                    rx: 0.0,
                    ry: 0.0,
                }),
                style: Arc::new(Style::default()),
            },
        )
        .expect("a rectangle"),
    )
}

fn layer(ids: &mut Ids, children: alloc::vec::Vec<Arc<Node>>) -> Arc<Node> {
    Arc::new(
        Node::new(
            ids.issue(),
            Kind::Group {
                children: Children::from_nodes(children).expect("held"),
                clip: None,
            },
        )
        .expect("a layer"),
    )
}

#[test]
fn a_node_is_found_and_replaced_by_its_path() {
    let mut ids = Ids::new();
    let target = rect(&mut ids, 30.0);
    let first = rect(&mut ids, 0.0);
    let inner = layer(&mut ids, vec![first, target.clone()]);
    let beside = rect(&mut ids, 100.0);
    let top = layer(&mut ids, vec![beside, inner]);
    let drawing = Drawing::new(
        page(),
        Children::from_nodes(vec![top]).expect("held"),
        SharedSlice::new(),
        Grid::default(),
    )
    .expect("a drawing");
    let path = drawing.find(target.id()).expect("searched").expect("found");
    assert_eq!(path, [0, 1, 1]);
    assert!(Arc::ptr_eq(drawing.node_at(&path).expect("there"), &target));
    let moved = Arc::new(
        target
            .with_transform(tairix_bezier::Affine::translate(500.0, 0.0))
            .expect("moved"),
    );
    let edited = drawing.with_node_at(&path, moved.clone()).expect("edited");
    assert!(Arc::ptr_eq(edited.node_at(&path).expect("there"), &moved));
    assert_eq!(
        edited.bounds().max.x,
        540.0,
        "every group above learns its new bounds"
    );
    assert_eq!(drawing.bounds().max.x, 110.0, "the original is unchanged");
    assert!(Arc::ptr_eq(
        edited.node_at(&[0, 0]).expect("there"),
        drawing.node_at(&[0, 0]).expect("there")
    ));
}

#[test]
fn many_nodes_are_found_in_one_walk() {
    let mut ids = Ids::new();
    let deep = rect(&mut ids, 30.0);
    let first = rect(&mut ids, 0.0);
    let inner = layer(&mut ids, vec![first.clone(), deep.clone()]);
    let beside = rect(&mut ids, 100.0);
    let top = layer(&mut ids, vec![beside.clone(), inner]);
    let drawing = Drawing::new(
        page(),
        Children::from_nodes(vec![top]).expect("held"),
        SharedSlice::new(),
        Grid::default(),
    )
    .expect("a drawing");
    let absent = ids.issue();
    let found = drawing
        .find_each(&[deep.id(), absent, beside.id(), deep.id(), first.id()])
        .expect("searched");
    let expected: [Option<&[usize]>; 5] = [
        Some(&[0, 1, 1]),
        None,
        Some(&[0, 0]),
        Some(&[0, 1, 1]),
        Some(&[0, 1, 0]),
    ];
    assert_eq!(found.len(), expected.len());
    for (found, expected) in found.iter().zip(expected) {
        assert_eq!(found.as_deref(), expected);
    }
    assert!(drawing.find_each(&[]).expect("searched").is_empty());
}

#[test]
fn a_drawing_refuses_what_it_cannot_hold() {
    let mut ids = Ids::new();
    let not_a_layer = Children::from_nodes(vec![rect(&mut ids, 0.0)]).expect("held");
    assert_eq!(
        Drawing::new(page(), not_a_layer, SharedSlice::new(), Grid::default()).err(),
        Some(DrawingError::Invalid)
    );
    let mut empty_page = page();
    empty_page.width = 0.0;
    assert_eq!(
        Drawing::new(
            empty_page,
            Children::new(),
            SharedSlice::new(),
            Grid::default()
        )
        .err(),
        Some(DrawingError::Invalid)
    );
}

#[test]
fn a_drawing_keeps_its_swatches_up_to_its_bound() {
    let drawing =
        Drawing::new(page(), Children::new(), SharedSlice::new(), Grid::default()).expect("blank");
    assert!(drawing.swatches().is_empty());
    let red = tairix_colour::Rgba::new(255, 0, 0, 255);
    let kept = drawing.with_swatches(slice(&[red])).expect("one swatch");
    assert_eq!(kept.swatches(), &[red]);
    assert!(
        !kept.is(&drawing),
        "a drawing keeping other swatches is another"
    );
    let too_many = vec![red; crate::limits::MAX_SWATCHES + 1];
    assert_eq!(
        drawing.with_swatches(slice(&too_many)).err(),
        Some(DrawingError::TooComplex)
    );
}

#[test]
fn units_convert_to_pixels() {
    assert!((Unit::In.pixels() - 96.0).abs() < 1e-12);
    assert!((Unit::Mm.pixels() * 25.4 - 96.0).abs() < 1e-9);
    assert_eq!(Unit::from_symbol("pt"), Some(Unit::Pt));
    assert_eq!(Unit::from_symbol("furlong"), None);
}

#[test]
fn a_page_turns_and_is_measured_again_keeping_its_size() {
    let upright = Page::of(&super::PAPER[0]);
    let landscape = upright.turned(true);
    assert_eq!((landscape.width, landscape.height), (297.0, 210.0));
    assert_eq!(landscape.turned(true), landscape, "already on its side");
    assert_eq!(landscape.turned(false), upright);
    let inches = upright.measured_in(Unit::In);
    assert_eq!(inches.unit, Unit::In);
    assert!(
        (inches.size().x - upright.size().x).abs() < 1e-9,
        "the same size"
    );
    assert!((inches.width - 210.0 / 25.4).abs() < 1e-12);
}

#[test]
fn a_drawing_is_copied_as_raster_at_a_resolution_it_may_hold() {
    let drawing =
        Drawing::new(page(), Children::new(), SharedSlice::new(), Grid::default()).expect("blank");
    assert_eq!(
        drawing.resolution(),
        super::PIXELS_PER_INCH,
        "its own pixels"
    );
    let fine = drawing.with_resolution(600).expect("600 dpi");
    assert_eq!(fine.resolution(), 600);
    assert!(!fine.is(&drawing));
    for refused in [0, crate::limits::MAX_RESOLUTION + 1] {
        assert_eq!(
            drawing.with_resolution(refused).err(),
            Some(DrawingError::Invalid)
        );
    }
    assert!(
        (Unit::Pc.pixels() - 16.0).abs() < 1e-12,
        "a pica is a sixth of an inch"
    );
}
