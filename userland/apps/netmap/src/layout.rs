//! The deterministic radial-tree layout.
//!
//! The local host sits at the origin; every other node hangs off it on
//! concentric rings by its distance in the graph, and siblings share their
//! parent's angular wedge in proportion to how much of the tree hangs below
//! each. Given the same map the result is byte-identical — children are ordered
//! by their stable [`NodeKey`](crate::NodeKey), never by insertion — which is
//! what makes the view reproducible and the layout testable. It is not a
//! force-directed simulation: a map wants a stable, legible arrangement, not
//! jitter.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;
use core::f64::consts::TAU;

use tairix_geometry::Point;
use tairix_util::mathf;

use crate::model::{Map, NodeId, NodeKind};

/// Graph-space distance between adjacent concentric rings.
pub const RING_STEP: i32 = 240;

/// Lay the map out in place, setting every node's position.
pub fn layout(map: &mut Map) {
    if map.is_empty() {
        return;
    }

    let positions = positions(map);

    for (id, pos) in positions {
        map.set_pos(id, pos);
    }
}

/// Every node's laid-out position, computed over a borrow of the map so the
/// ordering reads each node's key in place rather than a copy of it.
fn positions(map: &Map) -> BTreeMap<NodeId, Point> {
    let ids: Vec<NodeId> = map.nodes().iter().map(|n| n.id).collect();
    let key = |id: &NodeId| map.node(*id).map(|node| &node.key);
    let root = map
        .nodes()
        .iter()
        .find(|n| n.kind == NodeKind::LocalHost)
        .map_or(ids[0], |n| n.id);

    let mut adjacency: BTreeMap<NodeId, Vec<NodeId>> =
        ids.iter().map(|&id| (id, Vec::new())).collect();
    for edge in map.edges() {
        adjacency.entry(edge.from).or_default().push(edge.to);
        adjacency.entry(edge.to).or_default().push(edge.from);
    }
    // Sorted once, so the walk visits children by identity, never by the
    // order edges happened to be added.
    for neighbours in adjacency.values_mut() {
        neighbours.sort_unstable_by(|a, b| key(a).cmp(&key(b)));
        neighbours.dedup();
    }

    let (depth, children) = breadth_first_tree(root, &ids, &adjacency);
    let weights = subtree_weights(&ids, &depth, &children);
    radial_positions(root, &depth, &children, &weights)
}

/// The BFS spanning tree from `root` over key-sorted `adjacency`: each node's
/// depth, and each node's children in that deterministic order. Nodes unreachable from the
/// root become children of the root so they still appear.
fn breadth_first_tree(
    root: NodeId,
    ids: &[NodeId],
    adjacency: &BTreeMap<NodeId, Vec<NodeId>>,
) -> (BTreeMap<NodeId, u32>, BTreeMap<NodeId, Vec<NodeId>>) {
    let mut depth: BTreeMap<NodeId, u32> = BTreeMap::new();
    let mut children: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    let mut visited: BTreeSet<NodeId> = BTreeSet::new();
    let mut queue: VecDeque<NodeId> = VecDeque::new();

    depth.insert(root, 0);
    visited.insert(root);
    queue.push_back(root);

    while let Some(current) = queue.pop_front() {
        let current_depth = depth[&current];
        for &neighbour in adjacency.get(&current).into_iter().flatten() {
            if visited.insert(neighbour) {
                depth.insert(neighbour, current_depth + 1);
                children.entry(current).or_default().push(neighbour);
                queue.push_back(neighbour);
            }
        }
    }

    for &id in ids {
        if visited.insert(id) {
            depth.insert(id, 1);
            children.entry(root).or_default().push(id);
        }
    }

    (depth, children)
}

/// The leaf count under each node (a leaf counts one), computed deepest-first
/// so a node is always summed after its children.
fn subtree_weights(
    ids: &[NodeId],
    depth: &BTreeMap<NodeId, u32>,
    children: &BTreeMap<NodeId, Vec<NodeId>>,
) -> BTreeMap<NodeId, u64> {
    let mut weights: BTreeMap<NodeId, u64> = ids.iter().map(|&id| (id, 1)).collect();
    let mut order: Vec<NodeId> = ids.to_vec();
    order.sort_by_key(|id| core::cmp::Reverse(depth.get(id).copied().unwrap_or(0)));
    for id in order {
        if let Some(kids) = children.get(&id) {
            if !kids.is_empty() {
                let total: u64 = kids.iter().map(|kid| weights[kid]).sum();
                weights.insert(id, total);
            }
        }
    }
    weights
}

/// Place each node at `depth * RING_STEP` from the origin, partitioning each
/// node's angular wedge among its children in proportion to their weights.
fn radial_positions(
    root: NodeId,
    depth: &BTreeMap<NodeId, u32>,
    children: &BTreeMap<NodeId, Vec<NodeId>>,
    weights: &BTreeMap<NodeId, u64>,
) -> BTreeMap<NodeId, Point> {
    let mut positions: BTreeMap<NodeId, Point> = BTreeMap::new();
    let mut stack: Vec<(NodeId, f64, f64)> = Vec::new();
    stack.push((root, 0.0, TAU));

    while let Some((node, start, end)) = stack.pop() {
        let mid = f64::midpoint(start, end);
        let radius = f64::from(depth.get(&node).copied().unwrap_or(0)) * f64::from(RING_STEP);
        positions.insert(
            node,
            Point::new(
                round(radius * mathf::cos(mid)),
                round(radius * mathf::sin(mid)),
            ),
        );
        if let Some(kids) = children.get(&node) {
            let total: u64 = kids.iter().map(|kid| weights[kid]).sum();
            if total == 0 {
                continue;
            }
            let span = end - start;
            let mut cursor = start;
            for &kid in kids {
                let fraction = weight_fraction(weights[&kid], total);
                let next = cursor + span * fraction;
                stack.push((kid, cursor, next));
                cursor = next;
            }
        }
    }

    positions
}

/// One child's share of its parent's wedge. Leaf counts are bounded by the node
/// cap, so both are exact in `f64`.
#[allow(
    clippy::cast_precision_loss,
    reason = "leaf counts are bounded by MAX_NODES, far inside f64's exact integer range"
)]
fn weight_fraction(weight: u64, total: u64) -> f64 {
    weight as f64 / total as f64
}

/// Round a finite coordinate to the nearest integer, away from zero on a tie,
/// saturating at the ends of `i32`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a float-to-int `as` cast saturates, and every caller's value is a coordinate"
)]
pub(crate) fn round(value: f64) -> i32 {
    let adjusted = if value >= 0.0 {
        value + 0.5
    } else {
        value - 0.5
    };
    adjusted as i32
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
