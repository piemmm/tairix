//! A bounding volume hierarchy over bounded items — a scene's objects, or a
//! prototype's limbs, leaves and faces — so a ray tests the few items near
//! its path rather than every one.
//!
//! Built top down, each split chosen by the surface area heuristic over the
//! items binned along each axis (Wald, "On fast Construction of SAH-based
//! Bounding Volume Hierarchies", 2007), so a tree of tens of thousands of
//! leaves builds in linear time a level; walked nearer child first, so a
//! closest-hit query shrinks its reach early and skips what lies behind, and
//! an object at a time, so a walk can be left between objects and taken up
//! again, as a packet's rays wait for one another at a lawn. A node keeps its
//! box in single precision, rounded outward, which halves what a forest's
//! hierarchies hold and never culls a ray the exact box admits.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_util::{fallible, mathf};

use crate::shape::{reciprocal, Aabb};
use crate::vector::{above, below, real, share, Ray, Vec3};

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

/// An object as a hierarchy is built over it: which it is, its box padded,
/// and the box's middle.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Item {
    object: u32,
    bounds: Aabb,
    centre: Vec3,
}

impl Item {
    /// Object `object`, lying in `bounds`.
    pub(crate) fn new(object: u32, bounds: Aabb) -> Self {
        Self {
            object,
            bounds: bounds.padded(),
            centre: bounds.centre(),
        }
    }
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
    /// Visit every object whose box `ray` crosses nearer than the reach,
    /// nearer boxes first; `visit(object, reach)` tests one and answers how
    /// the walk goes on.
    pub(crate) fn walk(&self, ray: &Ray, reach: f64, mut visit: impl FnMut(u32, f64) -> Walk) {
        let mut reach = reach;
        let mut cursor = self.cursor(ray, reach);
        while let Some(object) = self.next(&mut cursor, ray, reach) {
            match visit(object, reach) {
                Walk::Within(shorter) => reach = shorter,
                Walk::Stop => return,
            }
        }
    }

    /// A walk of `ray` through the hierarchy, nearer than `reach`, to be
    /// taken an object at a time with [`Bvh::next`].
    pub(crate) fn cursor(&self, ray: &Ray, reach: f64) -> Cursor {
        let inverse = reciprocal(ray.dir);
        let crossed = self
            .nodes
            .first()
            .and_then(|root| root.bounds().entry(ray, inverse, reach));
        Cursor {
            inverse,
            stack: [(0, 0.0); MAX_DEPTH + 1],
            pending: 0,
            next: crossed.map(|_| 0),
            leaf: 0..0,
        }
    }

    /// The next object of `cursor`'s walk whose box `ray` crosses nearer
    /// than `reach`, the reach as it stands now.
    #[inline]
    pub(crate) fn next(&self, cursor: &mut Cursor, ray: &Ray, reach: f64) -> Option<u32> {
        let inverse = cursor.inverse;
        let entry = |index: usize| {
            self.nodes
                .get(index)
                .and_then(|node| node.bounds().entry(ray, inverse, reach))
        };
        loop {
            if let Some(at) = cursor.leaf.next() {
                return self.order.get(at).copied();
            }
            let mut node = match cursor.next.take() {
                Some(node) => node,
                None => loop {
                    cursor.pending = cursor.pending.checked_sub(1)?;
                    let &(deferred, entered) = cursor.stack.get(cursor.pending)?;
                    if entered <= reach {
                        break deferred;
                    }
                },
            };
            loop {
                let current = self.nodes.get(node)?;
                if current.count > 0 {
                    let start = current.start as usize;
                    let end = start + current.count as usize;
                    if self.order.get(start..end).is_some() {
                        cursor.leaf = start..end;
                    }
                    break;
                }
                let (first, second) = (node + 1, current.start as usize);
                node = match (entry(first), entry(second)) {
                    (Some(a), Some(b)) => {
                        let (near, far, far_entry) = if b < a {
                            (second, first, a)
                        } else {
                            (first, second, b)
                        };
                        if let Some(slot) = cursor.stack.get_mut(cursor.pending) {
                            *slot = (far, far_entry);
                            cursor.pending += 1;
                        }
                        near
                    }
                    (Some(_), None) => first,
                    (None, Some(_)) => second,
                    (None, None) => break,
                };
            }
        }
    }
}

/// Where a walk through a [`Bvh`] has got to, so it can be left between
/// objects and taken up again with [`Bvh::next`].
pub(crate) struct Cursor {
    inverse: Vec3,
    /// Each deferred child with where its box is entered, so it is skipped
    /// unopened once a nearer hit has shortened the reach past it.
    stack: [(usize, f64); MAX_DEPTH + 1],
    pending: usize,
    /// The node to open once the open leaf's objects are handed over.
    next: Option<usize>,
    leaf: Range<usize>,
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

/// A hierarchy being built: the items not yet laid out, the subtrees still
/// to split, laid out depth first as they are taken, and the one being laid
/// out where it has come to.
#[derive(Debug)]
pub(crate) struct Builder {
    items: Vec<Item>,
    nodes: Vec<Node>,
    order: Vec<u32>,
    pending: Vec<Task>,
    laying: Option<Laying>,
    /// The bins a node's items are sorted into along each axis, reserved
    /// once for every node it lays out.
    bins: Vec<[(Aabb, usize); BINS]>,
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

/// A subtree being laid out a pass over its items at a time, so a step
/// visits no more of them than its budget however many the subtree holds,
/// and the next of its items the pass takes.
#[derive(Copy, Clone, Debug)]
struct Laying {
    task: Task,
    next: usize,
    pass: Pass,
}

/// A pass over a subtree's items: its box and their middles gathered; then
/// the items binned along each axis their middles spread over, for its node;
/// then parted about the plane chosen, or listed as a leaf.
#[derive(Copy, Clone, Debug)]
enum Pass {
    Gathering {
        bounds: Aabb,
        centres: Aabb,
    },
    Binning {
        node: usize,
        area: f64,
        axes: [Option<Spread>; 3],
    },
    Parting {
        node: usize,
        axis: usize,
        plane: f64,
        cut: usize,
    },
    Listing,
}

/// Where items' middles begin along an axis, and how many bins span a unit
/// of it.
#[derive(Copy, Clone, Debug)]
struct Spread {
    low: f64,
    scale: f64,
}

impl Builder {
    /// A build over `bounds`, one per item; `None` when the heap will not
    /// hold it.
    pub(crate) fn new(bounds: &[(u32, Aabb)]) -> Option<Self> {
        Self::over(fallible::collected(
            bounds.len(),
            bounds
                .iter()
                .map(|&(object, bounds)| Item::new(object, bounds)),
        )?)
    }

    /// A build over `items`, taken as they stand; `None` when the heap will
    /// not hold it.
    pub(crate) fn over(items: Vec<Item>) -> Option<Self> {
        let mut builder = Self {
            nodes: Vec::new(),
            order: Vec::new(),
            pending: Vec::new(),
            laying: None,
            bins: Vec::new(),
            items,
        };
        let count = builder.items.len();
        if !(fallible::reserve(&mut builder.nodes, 2 * count)
            && fallible::reserve(&mut builder.order, count)
            && fallible::reserve(&mut builder.pending, 2 * MAX_DEPTH + 2)
            && fallible::reserve(&mut builder.bins, 3))
        {
            return None;
        }
        builder.bins.resize(3, [(Aabb::EMPTY, 0); BINS]);
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

    /// Lay out subtrees until about `budget` of their items have been
    /// visited; whether the hierarchy is whole.
    pub(crate) fn step(&mut self, budget: usize) -> bool {
        let mut left = budget.max(1);
        while left > 0 {
            let Some(laying) = self.laying.take().or_else(|| {
                self.pending.pop().map(|task| Laying {
                    task,
                    next: 0,
                    pass: Pass::Gathering {
                        bounds: Aabb::EMPTY,
                        centres: Aabb::EMPTY,
                    },
                })
            }) else {
                break;
            };
            let visited;
            (self.laying, visited) = self.lay(laying, left);
            left = left.saturating_sub(visited.max(1));
        }
        self.laying.is_none() && self.pending.is_empty()
    }

    /// The share of the items laid out in leaves so far.
    pub(crate) fn done(&self) -> f64 {
        share(self.order.len(), self.items.len())
    }

    /// The hierarchy, once whole.
    pub(crate) fn finish(self) -> Bvh {
        Bvh {
            nodes: self.nodes,
            order: self.order,
        }
    }

    /// Take `laying`'s passes on over at most `allowance` of its items: the
    /// laying where it stops, `None` once its subtree is laid out, and how
    /// many items were visited.
    fn lay(&mut self, mut laying: Laying, allowance: usize) -> (Option<Laying>, usize) {
        let Task { start, end, .. } = laying.task;
        let count = end - start;
        let mut visited = 0;
        loop {
            let Some(items) = self.items.get_mut(start..end) else {
                return (None, visited);
            };
            let from = laying.next;
            let to = from + (count - from).min(allowance - visited);
            let taken = items.get(from..to).unwrap_or_default();
            match &mut laying.pass {
                Pass::Gathering { bounds, centres } => {
                    for item in taken {
                        *bounds = bounds.union(item.bounds);
                        *centres = centres.including(item.centre);
                    }
                }
                Pass::Binning { axes, .. } => {
                    for item in taken {
                        for (axis, (spread, bins)) in
                            axes.iter().zip(self.bins.iter_mut()).enumerate()
                        {
                            if let Some(spread) = spread {
                                let bin = &mut bins[bin_of(item.centre.along(axis), *spread)];
                                bin.0 = bin.0.union(item.bounds);
                                bin.1 += 1;
                            }
                        }
                    }
                }
                Pass::Parting {
                    axis, plane, cut, ..
                } => {
                    for index in from..to {
                        if items
                            .get(index)
                            .is_some_and(|item| item.centre.along(*axis) < *plane)
                        {
                            items.swap(*cut, index);
                            *cut += 1;
                        }
                    }
                }
                Pass::Listing => self.order.extend(taken.iter().map(|item| item.object)),
            }
            visited += to - from;
            laying.next = to;
            if to < count {
                return (Some(laying), visited);
            }
            let Some(pass) = self.next_pass(&laying) else {
                return (None, visited);
            };
            laying = Laying {
                next: 0,
                pass,
                ..laying
            };
            if visited >= allowance {
                return (Some(laying), visited);
            }
        }
    }

    /// The pass after `laying`'s finished one, its node laid as the pass it
    /// finished decides; `None` once the subtree is laid out, its children
    /// queued if it was split.
    fn next_pass(&mut self, laying: &Laying) -> Option<Pass> {
        let task = laying.task;
        let count = task.end - task.start;
        match laying.pass {
            Pass::Gathering { bounds, centres } => {
                let node = self.nodes.len();
                self.nodes.push(Node::new(bounds));
                if let Some(parent) = task.second_of.and_then(|parent| self.nodes.get_mut(parent)) {
                    parent.start = u32::try_from(node).unwrap_or(u32::MAX);
                }
                if task.depth + 1 >= MAX_DEPTH || count <= 1 {
                    return Some(self.leaf(node, task));
                }
                let axes = [0, 1, 2].map(|axis| {
                    let (low, high) = (centres.min.along(axis), centres.max.along(axis));
                    (!(high.is_nan() || low.is_nan()) && high > low).then(|| Spread {
                        low,
                        scale: real(BINS) / (high - low),
                    })
                });
                if axes.iter().all(Option::is_none) {
                    return self.unparted(node, task);
                }
                self.bins.fill([(Aabb::EMPTY, 0); BINS]);
                Some(Pass::Binning {
                    node,
                    area: bounds.half_area().max(f64::MIN_POSITIVE),
                    axes,
                })
            }
            Pass::Binning { node, area, axes } => {
                let Some((cost, axis, plane)) = cheapest(&axes, &self.bins, area) else {
                    return self.unparted(node, task);
                };
                if count <= MAX_LEAF && cost >= real(count) {
                    return Some(self.leaf(node, task));
                }
                Some(Pass::Parting {
                    node,
                    axis,
                    plane,
                    cut: 0,
                })
            }
            Pass::Parting { node, cut, .. } => {
                let cut = if cut == 0 || cut == count {
                    count / 2
                } else {
                    cut
                };
                self.split(node, task, cut);
                None
            }
            Pass::Listing => None,
        }
    }

    /// Node `node`, which no plane parts `task`'s items for: every centre in
    /// one place, so a crowded node is halved as it lies.
    fn unparted(&mut self, node: usize, task: Task) -> Option<Pass> {
        let count = task.end - task.start;
        if count > MAX_LEAF {
            self.split(node, task, count / 2);
            return None;
        }
        Some(self.leaf(node, task))
    }

    /// Node `node` as the leaf of `task`'s items, listed next in the order.
    fn leaf(&mut self, node: usize, task: Task) -> Pass {
        let start = u32::try_from(self.order.len()).unwrap_or(u32::MAX);
        if let Some(node) = self.nodes.get_mut(node) {
            node.start = start;
            node.count = u32::try_from(task.end - task.start).unwrap_or(u32::MAX);
        }
        Pass::Listing
    }

    /// Node `node` split `cut` items into `task`'s, its children queued, the
    /// first to be taken next so each subtree lies just after its parent.
    fn split(&mut self, node: usize, task: Task, cut: usize) {
        let middle = task.start + cut;
        self.pending.push(Task {
            start: middle,
            end: task.end,
            depth: task.depth + 1,
            second_of: Some(node),
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

/// The cheapest plane to cut a node's items along, as the surface area
/// heuristic weighs the bins they fill along each axis `axes` spread over,
/// the node's box `area` broad: its cost, its axis, and where it lies; `None`
/// where no plane leaves items either side.
fn cheapest(
    axes: &[Option<Spread>; 3],
    bins: &[[(Aabb, usize); BINS]],
    area: f64,
) -> Option<(f64, usize, f64)> {
    let mut best: Option<(f64, usize, f64)> = None;
    for (axis, (spread, bins)) in axes.iter().zip(bins).enumerate() {
        let Some(spread) = spread else {
            continue;
        };
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
                best = Some((cost, axis, spread.low + real(bin + 1) / spread.scale));
            }
        }
    }
    best
}

/// Which of the bins `spread` lays out the value `at` falls in.
fn bin_of(at: f64, spread: Spread) -> usize {
    let place = mathf::floor((at - spread.low) * spread.scale).clamp(0.0, real(BINS - 1));
    usize::try_from(mathf::round_i32(place))
        .unwrap_or(0)
        .min(BINS - 1)
}

#[cfg(test)]
#[path = "bvh_tests.rs"]
mod tests;
