//! Unit tests for the chunked children.

#![allow(
    clippy::float_cmp,
    reason = "the bounds compared are sums of small integers, exact by construction"
)]

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::{Bounds, Point};

use tairix_fuzzseed::Prng;

use super::{Children, CHUNK_MAX, CHUNK_MIN};
use crate::id::Ids;
use crate::node::{Kind, Node};
use crate::shape::{RectShape, Shape};
use crate::style::Style;

fn square(ids: &mut Ids, x: f64) -> Arc<Node> {
    Arc::new(
        Node::new(
            ids.issue(),
            Kind::Shape {
                shape: Shape::Rect(RectShape {
                    x,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                    rx: 0.0,
                    ry: 0.0,
                }),
                style: Arc::new(Style::default()),
            },
        )
        .expect("a square"),
    )
}

fn row(ids: &mut Ids, count: u32) -> Vec<Arc<Node>> {
    (0..count)
        .map(|i| square(ids, f64::from(i) * 2.0))
        .collect()
}

#[test]
fn children_keep_their_order_across_chunks() {
    let mut ids = Ids::new();
    let nodes = row(&mut ids, 300);
    let children = Children::from_nodes(nodes.clone()).expect("held");
    assert_eq!(children.len(), 300);
    assert!(children.iter().zip(&nodes).all(|(a, b)| Arc::ptr_eq(a, b)));
    assert!(Arc::ptr_eq(children.get(250).expect("held"), &nodes[250]));
    assert!(children.get(300).is_none());
    assert_eq!(children.summary().objects, 300);
    let bounds = children.summary().bounds;
    assert_eq!(bounds.min, Point::new(0.0, 0.0));
    assert_eq!(bounds.max, Point::new(599.0, 1.0));
}

#[test]
fn an_edit_shares_every_chunk_it_does_not_touch() {
    let mut ids = Ids::new();
    let children = Children::from_nodes(row(&mut ids, 300)).expect("held");
    let replacement = square(&mut ids, 1000.0);
    let edited = children.replaced(10, replacement.clone()).expect("edited");
    assert!(Arc::ptr_eq(edited.get(10).expect("held"), &replacement));
    assert!(
        Arc::ptr_eq(&edited.chunks[3], &children.chunks[3]),
        "untouched chunks are shared"
    );
    assert!(!Arc::ptr_eq(&edited.chunks[0], &children.chunks[0]));
    assert_eq!(edited.summary().bounds.max.x, 1001.0);
    // The original is unchanged.
    assert_eq!(children.summary().bounds.max.x, 599.0);
}

#[test]
fn insertions_and_removals_land_where_asked() {
    let mut ids = Ids::new();
    let mut children = Children::new();
    let mut expected: Vec<Arc<Node>> = Vec::new();
    for i in 0..400_usize {
        let node = square(&mut ids, f64::from(u32::try_from(i).expect("small")));
        let at = (i * 7) % (expected.len() + 1);
        children = children.inserted(at, node.clone()).expect("inserted");
        expected.insert(at, node);
    }
    assert!(children
        .chunks
        .iter()
        .all(|chunk| chunk.nodes.len() <= CHUNK_MAX));
    assert!(children
        .iter()
        .zip(&expected)
        .all(|(a, b)| Arc::ptr_eq(a, b)));
    for i in (0..400).step_by(3).rev() {
        children = children.removed(i).expect("removed");
        expected.remove(i);
    }
    assert_eq!(children.len(), expected.len());
    assert!(children
        .iter()
        .zip(&expected)
        .all(|(a, b)| Arc::ptr_eq(a, b)));
    assert!(children
        .inserted(children.len() + 1, square(&mut ids, 0.0))
        .is_err());
    assert!(children.removed(children.len()).is_err());
}

#[test]
fn meeting_finds_exactly_the_children_an_area_touches() {
    let mut ids = Ids::new();
    let children = Children::from_nodes(row(&mut ids, 300)).expect("held");
    let area = Bounds::new(Point::new(100.5, 0.0), Point::new(104.5, 0.5));
    let found: Vec<usize> = children.meeting(area).map(|(index, _)| index).collect();
    assert_eq!(found, [50, 51, 52]);
    let everywhere: usize = children.meeting(children.summary().bounds).count();
    assert_eq!(everywhere, 300);
}

/// Whether `children` holds exactly `expected`, in order, every chunk within
/// its bounds.
fn holds(children: &Children, expected: &[Arc<Node>]) -> bool {
    let sized = children.chunks.iter().all(|chunk| {
        chunk.nodes.len() <= CHUNK_MAX
            && (children.chunks.len() == 1 || chunk.nodes.len() >= CHUNK_MIN)
    });
    sized
        && children.len() == expected.len()
        && children.summary().objects == expected.len()
        && children
            .iter()
            .zip(expected)
            .all(|(a, b)| Arc::ptr_eq(a, b))
}

#[test]
fn bulk_insertions_and_removals_match_a_list() {
    let mut ids = Ids::new();
    let mut prng = Prng::new(0x00d2_a3ed);
    let mut expected = row(&mut ids, 700);
    let mut children = Children::from_nodes(expected.clone()).expect("held");
    assert!(holds(&children, &expected));
    for _ in 0..200 {
        if prng.below(3) == 0 && !expected.is_empty() {
            let mut at: Vec<usize> = (0..prng.below(40))
                .map(|_| prng.below(expected.len()))
                .collect();
            at.sort_unstable();
            at.dedup();
            let replacements: Vec<(usize, Arc<Node>)> = at
                .iter()
                .map(|&index| (index, square(&mut ids, 0.0)))
                .collect();
            children = children
                .with_replaced(replacements.iter().cloned())
                .expect("replaced");
            for (index, node) in replacements {
                expected[index] = node;
            }
        } else if prng.below(2) == 0 && !expected.is_empty() {
            let mut gone: Vec<usize> = (0..prng.below(40))
                .map(|_| prng.below(expected.len()))
                .collect();
            gone.sort_unstable();
            gone.dedup();
            children = children.without(&gone).expect("removed");
            for &index in gone.iter().rev() {
                expected.remove(index);
            }
        } else {
            let mut at: Vec<usize> = (0..prng.below(40))
                .map(|_| prng.below(expected.len() + 1))
                .collect();
            at.sort_unstable();
            let insertions: Vec<(usize, Arc<Node>)> = at
                .iter()
                .map(|&index| (index, square(&mut ids, 0.0)))
                .collect();
            children = children
                .with_inserted(insertions.iter().cloned())
                .expect("inserted");
            for (offset, (index, node)) in insertions.into_iter().enumerate() {
                expected.insert(index + offset, node);
            }
        }
        assert!(holds(&children, &expected));
    }
}

#[test]
fn a_bulk_edit_shares_every_chunk_it_misses() {
    let mut ids = Ids::new();
    let children = Children::from_nodes(row(&mut ids, 640)).expect("held");
    assert_eq!(children.chunks.len(), 10);
    let fewer = children.without(&[200, 201]).expect("removed");
    let more = children
        .with_inserted([(200, square(&mut ids, 0.0)), (210, square(&mut ids, 0.0))])
        .expect("inserted");
    let other = children
        .with_replaced([(200, square(&mut ids, 0.0)), (255, square(&mut ids, 0.0))])
        .expect("replaced");
    assert_eq!(other.len(), 640);
    for edited in [&fewer, &more, &other] {
        for chunk in (0..10).filter(|&chunk| chunk != 3) {
            assert!(Arc::ptr_eq(&edited.chunks[chunk], &children.chunks[chunk]));
        }
        assert!(!Arc::ptr_eq(&edited.chunks[3], &children.chunks[3]));
    }
    let appended = children.appended(row(&mut ids, 100)).expect("appended");
    assert_eq!(appended.len(), 740);
    assert!((0..9).all(|chunk| Arc::ptr_eq(&appended.chunks[chunk], &children.chunks[chunk])));
}

#[test]
fn removals_leave_no_slivers() {
    let mut ids = Ids::new();
    let nodes = row(&mut ids, 640);
    let children = Children::from_nodes(nodes.clone()).expect("held");
    let gone: Vec<usize> = (0..60).collect();
    let fewer = children.without(&gone).expect("removed");
    assert!(holds(&fewer, &nodes[60..]));
    let last: Vec<usize> = (580..640).collect();
    let fewer = children.without(&last).expect("removed");
    assert!(holds(&fewer, &nodes[..580]));
    let all: Vec<usize> = (0..640).collect();
    assert!(children.without(&all).expect("emptied").is_empty());
}

#[test]
fn edits_out_of_order_or_past_the_end_are_refused() {
    let mut ids = Ids::new();
    let children = Children::from_nodes(row(&mut ids, 10)).expect("held");
    assert!(children.without(&[3, 3]).is_err());
    assert!(children.without(&[4, 2]).is_err());
    assert!(children.without(&[10]).is_err());
    let node = square(&mut ids, 0.0);
    assert!(children
        .with_inserted([(4, node.clone()), (2, node.clone())])
        .is_err());
    assert!(children.with_inserted([(11, node.clone())]).is_err());
    assert!(children
        .with_replaced([(4, node.clone()), (4, node.clone())])
        .is_err());
    assert!(children
        .with_replaced([(4, node.clone()), (2, node.clone())])
        .is_err());
    assert!(children.with_replaced([(10, node.clone())]).is_err());
    assert!(Children::new().with_inserted([(1, node.clone())]).is_err());
    let first = Children::new()
        .with_inserted([(0, node.clone())])
        .expect("inserted");
    assert!(holds(&first, &[node]));
}
