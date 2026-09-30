//! A bounding volume hierarchy over a scene's bounded objects, so a ray tests
//! the few objects near its path rather than every one.
//!
//! Built top down, each split chosen by the surface area heuristic over the
//! objects binned along each axis (Wald, "On fast Construction of SAH-based
//! Bounding Volume Hierarchies", 2007), so a forest of thousands of branches
//! builds in linear time a level; walked nearer child first, so a
//! closest-hit query shrinks its reach early and skips what lies behind.

use alloc::vec::Vec;

use tairix_util::{fallible, mathf};

use crate::shape::{reciprocal, Aabb};
use crate::vector::{real, Ray, Vec3};

/// How deep the tree may grow, and so how deep a walk's stack must be.
const MAX_DEPTH: usize = 40;

/// The most objects a leaf holds before a split is always tried.
const MAX_LEAF: usize = 4;

/// What testing a split costs next to testing one object, as the surface area
/// heuristic weighs them.
const TRAVERSAL_COST: f64 = 1.0;

#[derive(Copy, Clone, Debug)]
struct Node {
    bounds: Aabb,
    /// For a leaf, its first object in the order; for a split, its second
    /// child, the first being the node just after it.
    start: u32,
    /// How many objects a leaf holds; `0` for a split.
    count: u32,
}

#[derive(Copy, Clone, Debug)]
struct Item {
    object: u32,
    bounds: Aabb,
    centre: Vec3,
}

/// The hierarchy.
#[derive(Debug, Default)]
pub(crate) struct Bvh {
    nodes: Vec<Node>,
    order: Vec<u32>,
}

/// What a walk's visitor asks of the walk.
pub(crate) enum Walk {
    /// Go on, as far as `reach`.
    Within(f64),
    /// Stop: the query is answered.
    Stop,
}

impl Bvh {
    /// The hierarchy over `bounds`, one per object, or `None` when the heap
    /// will not hold it.
    pub(crate) fn build(bounds: &[(u32, Aabb)]) -> Option<Self> {
        let mut items: Vec<Item> = fallible::collected(
            bounds.len(),
            bounds.iter().map(|&(object, bounds)| Item {
                object,
                bounds: bounds.padded(),
                centre: bounds.centre(),
            }),
        )?;
        let mut bvh = Self {
            nodes: Vec::new(),
            order: Vec::new(),
        };
        if items.is_empty() {
            return Some(bvh);
        }
        if !(fallible::reserve(&mut bvh.nodes, 2 * items.len())
            && fallible::reserve(&mut bvh.order, items.len()))
        {
            return None;
        }
        bvh.split(&mut items, 0);
        Some(bvh)
    }

    /// Lay out the subtree over `items`, answering its root's index.
    fn split(&mut self, items: &mut [Item], depth: usize) -> usize {
        let bounds = items
            .iter()
            .fold(Aabb::EMPTY, |bounds, item| bounds.union(item.bounds));
        let at = self.nodes.len();
        self.nodes.push(Node {
            bounds,
            start: 0,
            count: 0,
        });
        let cut = if depth + 1 >= MAX_DEPTH {
            None
        } else {
            best_cut(items, bounds)
        };
        let Some(cut) = cut else {
            self.make_leaf(at, items);
            return at;
        };
        let (left, right) = items.split_at_mut(cut);
        self.split(left, depth + 1);
        let second = self.split(right, depth + 1);
        if let Some(node) = self.nodes.get_mut(at) {
            node.start = u32::try_from(second).unwrap_or(u32::MAX);
        }
        at
    }

    fn make_leaf(&mut self, at: usize, items: &[Item]) {
        let start = u32::try_from(self.order.len()).unwrap_or(u32::MAX);
        self.order.extend(items.iter().map(|item| item.object));
        if let Some(node) = self.nodes.get_mut(at) {
            node.start = start;
            node.count = u32::try_from(items.len()).unwrap_or(u32::MAX);
        }
    }

    /// Visit every object whose box `ray` crosses nearer than the reach,
    /// nearer boxes first; `visit(object, reach)` tests one and answers how
    /// the walk goes on.
    pub(crate) fn walk(&self, ray: &Ray, reach: f64, mut visit: impl FnMut(u32, f64) -> Walk) {
        let inverse = reciprocal(ray.dir);
        let mut reach = reach;
        let entry = |index: usize, reach: f64| {
            self.nodes
                .get(index)
                .and_then(|node| node.bounds.entry(ray, inverse, reach))
        };
        if entry(0, reach).is_none() {
            return;
        }
        // Each deferred child with where its box is entered, so it is skipped
        // unopened once a nearer hit has shortened the reach past it.
        let mut stack = [(0usize, 0.0f64); MAX_DEPTH + 1];
        let mut pending = 0usize;
        let mut node = 0usize;
        loop {
            let Some(current) = self.nodes.get(node) else {
                return;
            };
            if current.count > 0 {
                let start = current.start as usize;
                let end = start + current.count as usize;
                for &object in self.order.get(start..end).unwrap_or(&[]) {
                    match visit(object, reach) {
                        Walk::Within(shorter) => reach = shorter,
                        Walk::Stop => return,
                    }
                }
            } else {
                let (first, second) = (node + 1, current.start as usize);
                let next = match (entry(first, reach), entry(second, reach)) {
                    (Some(a), Some(b)) => {
                        let (near, far, far_entry) = if b < a {
                            (second, first, a)
                        } else {
                            (first, second, b)
                        };
                        if let Some(slot) = stack.get_mut(pending) {
                            *slot = (far, far_entry);
                            pending += 1;
                        }
                        Some(near)
                    }
                    (Some(_), None) => Some(first),
                    (None, Some(_)) => Some(second),
                    (None, None) => None,
                };
                if let Some(next) = next {
                    node = next;
                    continue;
                }
            }
            loop {
                if pending == 0 {
                    return;
                }
                pending -= 1;
                let Some(&(deferred, entered)) = stack.get(pending) else {
                    return;
                };
                if entered <= reach {
                    node = deferred;
                    break;
                }
            }
        }
    }
}

/// How many bins the objects of a node are sorted into along each axis.
const BINS: usize = 16;

/// Where to cut `items` in two, having partitioned them about the chosen
/// plane, or `None` when keeping them in one leaf costs no more than any cut.
fn best_cut(items: &mut [Item], bounds: Aabb) -> Option<usize> {
    let count = items.len();
    if count <= 1 {
        return None;
    }
    let centres = items
        .iter()
        .fold(Aabb::EMPTY, |bounds, item| bounds.including(item.centre));
    let area = bounds.half_area().max(f64::MIN_POSITIVE);
    let mut best: Option<(f64, usize, f64)> = None;
    for axis in 0..3 {
        let (low, high) = (centres.min.along(axis), centres.max.along(axis));
        if high.is_nan() || low.is_nan() || high <= low {
            continue;
        }
        let scale = real(BINS) / (high - low);
        let mut bins = [(Aabb::EMPTY, 0usize); BINS];
        for item in items.iter() {
            let bin = &mut bins[bin_of(item.centre.along(axis), low, scale)];
            bin.0 = bin.0.union(item.bounds);
            bin.1 += 1;
        }
        // What lies above each boundary between bins, swept from the top.
        let mut above = [(0.0, 0usize); BINS];
        let mut sweep = (Aabb::EMPTY, 0usize);
        for bin in (1..BINS).rev() {
            sweep = (sweep.0.union(bins[bin].0), sweep.1 + bins[bin].1);
            above[bin] = (sweep.0.half_area(), sweep.1);
        }
        let mut below = (Aabb::EMPTY, 0usize);
        for bin in 0..BINS - 1 {
            below = (below.0.union(bins[bin].0), below.1 + bins[bin].1);
            let (right_area, right) = above[bin + 1];
            if below.1 == 0 || right == 0 {
                continue;
            }
            let cost = TRAVERSAL_COST
                + (below.0.half_area() * real(below.1) + right_area * real(right)) / area;
            if best.is_none_or(|(held, _, _)| cost < held) {
                best = Some((cost, axis, low + real(bin + 1) / scale));
            }
        }
    }
    let Some((cost, axis, plane)) = best else {
        // Every centre in one place: no plane parts them, so a crowded node
        // is halved as it lies.
        return (count > MAX_LEAF).then_some(count / 2);
    };
    if count <= MAX_LEAF && cost >= real(count) {
        return None;
    }
    let cut = partition(items, |item| item.centre.along(axis) < plane);
    Some(if cut == 0 || cut == count {
        count / 2
    } else {
        cut
    })
}

/// Which of the bins `scale` to a unit from `low` the value `at` falls in.
fn bin_of(at: f64, low: f64, scale: f64) -> usize {
    let place = mathf::floor((at - low) * scale).clamp(0.0, real(BINS - 1));
    usize::try_from(mathf::round_i32(place))
        .unwrap_or(0)
        .min(BINS - 1)
}

/// Move every item `below` holds for to the front, answering how many.
fn partition(items: &mut [Item], below: impl Fn(&Item) -> bool) -> usize {
    let mut cut = 0;
    for index in 0..items.len() {
        if items.get(index).is_some_and(&below) {
            items.swap(cut, index);
            cut += 1;
        }
    }
    cut
}

#[cfg(test)]
#[path = "bvh_tests.rs"]
mod tests;
