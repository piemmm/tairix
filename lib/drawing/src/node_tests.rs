//! Unit tests for nodes.

#![allow(
    clippy::float_cmp,
    reason = "the bounds compared are sums of small integers, exact by construction"
)]

use alloc::sync::Arc;
use alloc::vec;

use tairix_bezier::{Affine, Point};

use super::{Kind, Node};
use crate::children::Children;
use crate::id::Ids;
use crate::limits::{MAX_DEPTH, MAX_NAME_BYTES};
use crate::shape::{RectShape, Shape};
use crate::style::{Paint, Style};
use crate::testing::shared;
use crate::DrawingError;

fn rect(ids: &mut Ids, x: f64, width: f64) -> Node {
    Node::new(
        ids.issue(),
        Kind::Shape {
            shape: Shape::Rect(RectShape {
                x,
                y: 0.0,
                width,
                height: 10.0,
                rx: 0.0,
                ry: 0.0,
            }),
            style: Arc::new(Style::default()),
        },
    )
    .expect("a rectangle")
}

fn group(
    ids: &mut Ids,
    children: alloc::vec::Vec<Node>,
    clip: Option<Node>,
) -> Result<Node, DrawingError> {
    Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(children.into_iter().map(Arc::new).collect())?,
            clip: clip.map(Arc::new),
        },
    )
}

#[test]
fn bounds_follow_the_transform() {
    let mut ids = Ids::new();
    let node = rect(&mut ids, 0.0, 10.0);
    assert_eq!(node.bounds().max, Point::new(10.0, 10.0));
    let moved = node
        .with_transform(Affine::translate(5.0, 7.0))
        .expect("moved");
    assert_eq!(moved.bounds().min, Point::new(5.0, 7.0));
    assert_eq!(moved.local_bounds(), node.local_bounds());
    let mut stroked = Style::default();
    stroked.stroke.paint = Paint::Colour(tairix_colour::Rgba::new(0, 0, 0, 255));
    stroked.stroke.width = 4.0;
    let thick = node
        .with_kind(Kind::Shape {
            shape: Shape::Rect(RectShape {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
                rx: 0.0,
                ry: 0.0,
            }),
            style: Arc::new(stroked),
        })
        .expect("stroked");
    assert!(
        thick.bounds().min.x <= -2.0,
        "the stroke reaches past the outline"
    );
}

#[test]
fn a_group_holds_its_children_and_is_clipped_by_its_clip() {
    let mut ids = Ids::new();
    let children = vec![rect(&mut ids, 0.0, 10.0), rect(&mut ids, 50.0, 10.0)];
    let plain = group(&mut ids, children.clone(), None).expect("a group");
    assert_eq!(plain.objects(), 3);
    assert_eq!(plain.depth(), 2);
    assert_eq!(plain.bounds().max.x, 60.0);
    let clip = rect(&mut ids, 0.0, 20.0);
    let clipped = group(&mut ids, children, Some(clip)).expect("a clipped group");
    assert_eq!(
        clipped.bounds().max.x,
        20.0,
        "only what the clip covers shows"
    );
    assert_eq!(clipped.objects(), 4);
}

#[test]
fn nesting_past_the_bound_is_refused() {
    let mut ids = Ids::new();
    let mut node = rect(&mut ids, 0.0, 1.0);
    for _ in 1..MAX_DEPTH {
        node = group(&mut ids, vec![node], None).expect("within the bound");
    }
    assert_eq!(node.depth(), MAX_DEPTH);
    assert_eq!(
        group(&mut ids, vec![node], None).err(),
        Some(DrawingError::TooComplex)
    );
}

#[test]
fn names_opacities_and_transforms_are_checked() {
    let mut ids = Ids::new();
    let node = rect(&mut ids, 0.0, 1.0);
    assert!(node.with_name(Some(shared("Sun"))).is_ok());
    assert_eq!(
        node.with_name(Some(shared(""))).err(),
        Some(DrawingError::Invalid)
    );
    assert_eq!(
        node.with_name(Some(shared("a\u{1}b"))).err(),
        Some(DrawingError::Invalid)
    );
    let long: alloc::string::String = core::iter::repeat_n('x', MAX_NAME_BYTES + 1).collect();
    assert_eq!(
        node.with_name(Some(shared(&long))).err(),
        Some(DrawingError::TooComplex)
    );
    assert_eq!(node.with_opacity(1.5).err(), Some(DrawingError::Invalid));
    assert_eq!(
        node.with_transform(Affine::scale(f64::NAN, 1.0)).err(),
        Some(DrawingError::Invalid)
    );
    assert!(node.with_locked(true).locked());
    assert!(!node.with_visible(false).visible());
}
