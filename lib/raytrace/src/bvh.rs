//! A bounding volume hierarchy over bounded items — a scene's objects, or a
//! prototype's limbs, leaves and faces — so a ray tests the few items near
//! its path rather than every one.
//!
//! Built top down, each split chosen by the surface area heuristic over the
//! items binned along each axis (Wald, "On fast Construction of SAH-based
//! Bounding Volume Hierarchies", 2007), so a tree of tens of thousands of
//! leaves builds in linear time a level; walked nearer child first, so a
//! closest-hit query shrinks its reach early and skips what lies behind. A
//! node keeps its box in single precision, rounded outward, which halves what
//! a forest's hierarchies hold and never culls a ray the exact box admits.

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
    /// The box's corners, rounded outward.
    low: [f32; 3],
    high: [f32; 3],
    /// For a leaf, its first item in the order; for a split, its second
    /// child, the first being the node just after it.
    start: u32,
    /// How many items a leaf holds; `0` for a split.
    count: u32,
}

impl Node {
    fn new(bounds: Aabb) -> Self {
        let (min, max) = (bounds.min, bounds.max);
        Self {
            low: [below(min.x), below(min.y), below(min.z)],
            high: [above(max.x), above(max.y), above(max.z)],
            start: 0,
            count: 0,
        }
    }

    fn bounds(&self) -> Aabb {
        let [lx, ly, lz] = self.low.map(f64::from);
        let [hx, hy, hz] = self.high.map(f64::from);
        Aabb {
            min: Vec3::new(lx, ly, lz),
            max: Vec3::new(hx, hy, hz),
        }
    }

    fn holds(&self, point: Vec3) -> bool {
        let bounds = self.bounds();
        (0..3).all(|axis| {
            (bounds.min.along(axis)..=bounds.max.along(axis)).contains(&point.along(axis))
        })
    }
}

/// The nearest single-precision value at or below `value`.
fn below(value: f64) -> f32 {
    let near = narrow(value);
    if f64::from(near) > value {
        near.next_down()
    } else {
        near
    }
}

/// The nearest single-precision value at or above `value`.
fn above(value: f64) -> f32 {
    let near = narrow(value);
    if f64::from(near) < value {
        near.next_up()
    } else {
        near
    }
}

/// `value` rounded to single precision, saturating at its range; the callers
/// step it outward.
fn narrow(value: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "rounded to nearest and then stepped outward by the caller"
    )]
    {
        value as f32
    }
}

#[derive(Copy, Clone, Debug)]
struct Item {
    object: u32,
    bounds: Aabb,
    centre: Vec3,
}

/// The hierarchy.
#[derive(Clone, Debug, Default)]
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
    /// The hierarchy over `bounds`, one per item, or `None` when the heap will
    /// not hold it.
    pub(crate) fn build(bounds: &[(u32, Aabb)]) -> Option<Self> {
        let mut builder = Builder::new(bounds)?;
        builder.step(usize::MAX);
        Some(builder.finish())
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
                .and_then(|node| node.bounds().entry(ray, inverse, reach))
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

impl Bvh {
    /// Visit every item whose box holds `point`.
    pub(crate) fn containing(&self, point: Vec3, mut visit: impl FnMut(u32)) {
        let holds = |index: usize| self.nodes.get(index).is_some_and(|node| node.holds(point));
        if !holds(0) {
            return;
        }
        let mut stack = [0usize; MAX_DEPTH + 1];
        let mut pending = 0usize;
        let mut node = 0usize;
        loop {
            let Some(current) = self.nodes.get(node) else {
                return;
            };
            let mut next = None;
            if current.count > 0 {
                let start = current.start as usize;
                let end = start + current.count as usize;
                for &object in self.order.get(start..end).unwrap_or(&[]) {
                    visit(object);
                }
            } else {
                let (first, second) = (node + 1, current.start as usize);
                next = match (holds(first), holds(second)) {
                    (true, true) => {
                        if let Some(slot) = stack.get_mut(pending) {
                            *slot = second;
                            pending += 1;
                        }
                        Some(first)
                    }
                    (true, false) => Some(first),
                    (false, true) => Some(second),
                    (false, false) => None,
                };
            }
            if let Some(next) = next {
                node = next;
                continue;
            }
            if pending == 0 {
                return;
            }
            pending -= 1;
            let Some(&deferred) = stack.get(pending) else {
                return;
            };
            node = deferred;
        }
    }
}

/// A hierarchy being built: the items not yet laid out, and the subtrees
/// still to split, laid out depth first as they are taken.
#[derive(Debug)]
pub(crate) struct Builder {
    items: Vec<Item>,
    nodes: Vec<Node>,
    order: Vec<u32>,
    pending: Vec<Task>,
}

/// A subtree still to lay out: its items, how deep it lies, and the node it
/// is the second child of, if it is one.
#[derive(Copy, Clone, Debug)]
struct Task {
    start: usize,
    end: usize,
    depth: usize,
    second_of: Option<usize>,
}

impl Builder {
    /// A build over `bounds`, one per item; `None` when the heap will not
    /// hold it.
    pub(crate) fn new(bounds: &[(u32, Aabb)]) -> Option<Self> {
        let items: Vec<Item> = fallible::collected(
            bounds.len(),
            bounds.iter().map(|&(object, bounds)| Item {
                object,
                bounds: bounds.padded(),
                centre: bounds.centre(),
            }),
        )?;
        let mut builder = Self {
            nodes: Vec::new(),
            order: Vec::new(),
            pending: Vec::new(),
            items,
        };
        let count = builder.items.len();
        if !(fallible::reserve(&mut builder.nodes, 2 * count)
            && fallible::reserve(&mut builder.order, count)
            && fallible::reserve(&mut builder.pending, 2 * MAX_DEPTH + 2))
        {
            return None;
        }
        if count > 0 {
            builder.pending.push(Task {
                start: 0,
                end: count,
                depth: 0,
                second_of: None,
            });
        }
        Some(builder)
    }

    /// Lay out subtrees until about `budget` items have been sorted between
    /// children; whether the hierarchy is whole.
    pub(crate) fn step(&mut self, budget: usize) -> bool {
        let mut spent = 0usize;
        while let Some(task) = self.pending.pop() {
            spent = spent.saturating_add(task.end - task.start);
            self.lay_out(task);
            if spent >= budget {
                break;
            }
        }
        self.pending.is_empty()
    }

    /// The hierarchy, once whole.
    pub(crate) fn finish(self) -> Bvh {
        Bvh {
            nodes: self.nodes,
            order: self.order,
        }
    }

    /// Lay out `task`'s node, and queue its children, the first to be taken
    /// next so each subtree lies just after its parent.
    fn lay_out(&mut self, task: Task) {
        let Some(items) = self.items.get_mut(task.start..task.end) else {
            return;
        };
        let bounds = items
            .iter()
            .fold(Aabb::EMPTY, |bounds, item| bounds.union(item.bounds));
        let at = self.nodes.len();
        self.nodes.push(Node::new(bounds));
        if let Some(parent) = task.second_of.and_then(|parent| self.nodes.get_mut(parent)) {
            parent.start = u32::try_from(at).unwrap_or(u32::MAX);
        }
        let cut = if task.depth + 1 >= MAX_DEPTH {
            None
        } else {
            best_cut(items, bounds)
        };
        let Some(cut) = cut else {
            let start = u32::try_from(self.order.len()).unwrap_or(u32::MAX);
            self.order.extend(items.iter().map(|item| item.object));
            if let Some(node) = self.nodes.get_mut(at) {
                node.start = start;
                node.count = u32::try_from(task.end - task.start).unwrap_or(u32::MAX);
            }
            return;
        };
        let middle = task.start + cut;
        self.pending.push(Task {
            start: middle,
            end: task.end,
            depth: task.depth + 1,
            second_of: Some(at),
        });
        self.pending.push(Task {
            start: task.start,
            end: middle,
            depth: task.depth + 1,
            second_of: None,
        });
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
