//! Dominator trees, by Lengauer and Tarjan's algorithm with path compression
//! (A Fast Algorithm for Finding Dominators in a Flowgraph, TOPLAS 1979):
//! O(m log n) on any graph, where the iterative data-flow method is quadratic
//! on a hostile one. Every walk is iterative, so a graph as deep as a module
//! may make costs no stack.
//!
//! A tree's nodes are numbered by a depth-first walk, so whether one node
//! dominates another is two comparisons. Graphs and trees are rebuilt in the
//! memory they already hold, so the many functions of a module share it.

use alloc::vec::Vec;

use super::{refill, reserve, Refusal, Rule, NONE};

/// A directed graph over nodes `0..n`, its successor lists packed.
#[derive(Debug, Default)]
pub(crate) struct Graph {
    starts: Vec<u32>,
    edges: Vec<u32>,
}

impl Graph {
    /// Become the graph of `n` nodes whose edges `edges` yields, each as
    /// `(from, to)`, in any order, with `next` as scratch; an edge naming no
    /// node is refused.
    pub(crate) fn rebuild(
        &mut self,
        n: usize,
        edges: impl Iterator<Item = (u32, u32)> + Clone,
        next: &mut Vec<u32>,
    ) -> Result<(), Refusal> {
        refill(&mut self.starts, n + 1, 0)?;
        let mut count = 0usize;
        for (from, to) in edges.clone() {
            if from as usize >= n || to as usize >= n {
                return Err(Rule::Block.module());
            }
            self.starts[from as usize + 1] += 1;
            count += 1;
        }
        for i in 1..=n {
            self.starts[i] += self.starts[i - 1];
        }
        next.clear();
        reserve(next, n)?;
        next.extend_from_slice(&self.starts[..n]);
        refill(&mut self.edges, count, 0)?;
        for (from, to) in edges {
            let slot = &mut next[from as usize];
            self.edges[*slot as usize] = to;
            *slot += 1;
        }
        Ok(())
    }

    pub(crate) fn len(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }

    pub(crate) fn successors(&self, node: u32) -> &[u32] {
        let node = node as usize;
        match (self.starts.get(node), self.starts.get(node + 1)) {
            (Some(&start), Some(&end)) => {
                self.edges.get(start as usize..end as usize).unwrap_or(&[])
            }
            _ => &[],
        }
    }

    /// Make `reversed` this graph with every edge reversed, each node's list
    /// ascending and without repeats.
    pub(crate) fn reverse_into(
        &self,
        reversed: &mut Self,
        next: &mut Vec<u32>,
    ) -> Result<(), Refusal> {
        let n = self.len();
        let edges = (0..n).flat_map(move |from| {
            self.successors(u32::try_from(from).unwrap_or(NONE))
                .iter()
                .map(move |&to| (to, u32::try_from(from).unwrap_or(NONE)))
        });
        reversed.rebuild(n, edges, next)?;
        // Edges were placed in ascending order of their source, so each
        // node's repeats are adjacent.
        let mut kept = 0u32;
        let mut start = 0usize;
        for node in 0..n {
            let end = reversed.starts[node + 1] as usize;
            let first = kept;
            reversed.starts[node] = first;
            for read in start..end {
                let to = reversed.edges[read];
                if kept == first || reversed.edges[kept as usize - 1] != to {
                    reversed.edges[kept as usize] = to;
                    kept += 1;
                }
            }
            start = end;
        }
        reversed.starts[n] = kept;
        reversed.edges.truncate(kept as usize);
        Ok(())
    }
}

/// The dominator tree of a graph from a root.
#[derive(Debug, Default)]
pub(crate) struct Dominators {
    /// Each node's immediate dominator; [`NONE`] for the root and for every
    /// node the root does not reach.
    idom: Vec<u32>,
    /// Each node's interval in a depth-first walk of the tree; [`NONE`] for
    /// a node the root does not reach.
    enter: Vec<u32>,
    exit: Vec<u32>,
    /// The reached nodes by their entry in that walk.
    order: Vec<u32>,
}

/// The tables a dominator tree is computed in, kept between computations.
#[derive(Debug, Default)]
pub(crate) struct Work {
    number: Vec<u32>,
    vertex: Vec<u32>,
    parent: Vec<u32>,
    stack: Vec<(u32, u32)>,
    semi: Vec<u32>,
    ancestor: Vec<u32>,
    label: Vec<u32>,
    bucket_head: Vec<u32>,
    next_in_bucket: Vec<u32>,
    path: Vec<u32>,
    tree: Graph,
    pub(crate) next: Vec<u32>,
}

impl Dominators {
    /// Become the dominator tree of `graph`, whose predecessor lists are
    /// `predecessors`, from `root`.
    pub(crate) fn rebuild(
        &mut self,
        graph: &Graph,
        predecessors: &Graph,
        root: u32,
        work: &mut Work,
    ) -> Result<(), Refusal> {
        let nodes = graph.len();
        let Work {
            number,
            vertex,
            parent,
            stack,
            semi,
            ancestor,
            label,
            bucket_head,
            next_in_bucket,
            path,
            tree,
            next,
        } = work;
        // The depth-first numbering: `number[v]` is v's, `vertex[i]` the
        // node numbered i, `parent[v]` its parent in the walk.
        refill(number, nodes, NONE)?;
        vertex.clear();
        reserve(vertex, nodes)?;
        refill(parent, nodes, NONE)?;
        stack.clear();
        reserve(stack, nodes)?;
        if (root as usize) < nodes {
            number[root as usize] = 0;
            vertex.push(root);
            stack.push((root, 0));
        }
        while let Some(&(node, next)) = stack.last() {
            let Some(&successor) = graph.successors(node).get(next as usize) else {
                stack.pop();
                continue;
            };
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            if number[successor as usize] == NONE {
                number[successor as usize] = super::scan::len_u32(vertex);
                vertex.push(successor);
                parent[successor as usize] = node;
                stack.push((successor, 0));
            }
        }
        let reached = vertex.len();
        refill(semi, nodes, NONE)?;
        refill(&mut self.idom, nodes, NONE)?;
        refill(ancestor, nodes, NONE)?;
        refill(label, nodes, NONE)?;
        for &node in vertex.iter() {
            semi[node as usize] = number[node as usize];
            label[node as usize] = node;
        }
        // Each bucket is threaded through `next_in_bucket`, headed at
        // `bucket_head`.
        refill(bucket_head, nodes, NONE)?;
        refill(next_in_bucket, nodes, NONE)?;
        path.clear();
        reserve(path, nodes)?;
        let idom = &mut self.idom;
        for i in (1..reached).rev() {
            let node = vertex[i];
            for &before in predecessors.successors(node) {
                if number[before as usize] == NONE {
                    continue;
                }
                let least = eval(before, ancestor, label, semi, path);
                if semi[least as usize] < semi[node as usize] {
                    semi[node as usize] = semi[least as usize];
                }
            }
            let semidominator = vertex[semi[node as usize] as usize];
            next_in_bucket[node as usize] = bucket_head[semidominator as usize];
            bucket_head[semidominator as usize] = node;
            let up = parent[node as usize];
            ancestor[node as usize] = up;
            let mut waiting = bucket_head[up as usize];
            bucket_head[up as usize] = NONE;
            while waiting != NONE {
                let least = eval(waiting, ancestor, label, semi, path);
                idom[waiting as usize] = if semi[least as usize] < semi[waiting as usize] {
                    least
                } else {
                    up
                };
                waiting = next_in_bucket[waiting as usize];
            }
        }
        for &node in vertex.iter().skip(1) {
            let immediate = idom[node as usize];
            if immediate != NONE && immediate != vertex[semi[node as usize] as usize] {
                idom[node as usize] = idom[immediate as usize];
            }
        }
        self.intervals(nodes, root, vertex, tree, stack, next)
    }

    /// Number each reached node's entry and exit in a depth-first walk of
    /// the tree `idom` gives, and list the nodes in that walk's order.
    fn intervals(
        &mut self,
        n: usize,
        root: u32,
        vertex: &[u32],
        tree: &mut Graph,
        stack: &mut Vec<(u32, u32)>,
        next: &mut Vec<u32>,
    ) -> Result<(), Refusal> {
        let idom = &self.idom;
        tree.rebuild(
            n,
            vertex.iter().filter_map(|&v| {
                idom.get(v as usize)
                    .filter(|&&d| d != NONE)
                    .map(|&d| (d, v))
            }),
            next,
        )?;
        refill(&mut self.enter, n, NONE)?;
        refill(&mut self.exit, n, NONE)?;
        let (enter, exit, order) = (&mut self.enter, &mut self.exit, &mut self.order);
        order.clear();
        reserve(order, vertex.len())?;
        stack.clear();
        if (root as usize) < n && !vertex.is_empty() {
            enter[root as usize] = 0;
            order.push(root);
            stack.push((root, 0));
        }
        while let Some(&(node, next)) = stack.last() {
            let Some(&child) = tree.successors(node).get(next as usize) else {
                exit[node as usize] = super::scan::len_u32(order) - 1;
                stack.pop();
                continue;
            };
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            enter[child as usize] = super::scan::len_u32(order);
            order.push(child);
            stack.push((child, 0));
        }
        Ok(())
    }

    /// Whether the root reaches `node`.
    pub(crate) fn reaches(&self, node: u32) -> bool {
        self.enter.get(node as usize).is_some_and(|&e| e != NONE)
    }

    /// `node`'s immediate dominator, unless it is the root or unreached.
    pub(crate) fn idom(&self, node: u32) -> Option<u32> {
        self.idom.get(node as usize).copied().filter(|&d| d != NONE)
    }

    /// Whether `a` dominates `b`; both reached.
    pub(crate) fn dominates(&self, a: u32, b: u32) -> bool {
        match (
            self.enter.get(a as usize),
            self.exit.get(a as usize),
            self.enter.get(b as usize),
        ) {
            (Some(&ea), Some(&xa), Some(&eb)) if ea != NONE && eb != NONE => ea <= eb && eb <= xa,
            _ => false,
        }
    }

    /// Whether `a` dominates `b` and is not `b`.
    pub(crate) fn strictly_dominates(&self, a: u32, b: u32) -> bool {
        a != b && self.dominates(a, b)
    }

    /// The reached nodes in a depth-first order of the tree: each after its
    /// immediate dominator.
    pub(crate) fn preorder(&self) -> &[u32] {
        &self.order
    }
}

/// The node with the smallest semidominator on `v`'s path to the root of its
/// tree in the forest, compressing the path as it goes.
fn eval(v: u32, ancestor: &mut [u32], label: &mut [u32], semi: &[u32], path: &mut Vec<u32>) -> u32 {
    if ancestor[v as usize] == NONE {
        return v;
    }
    path.clear();
    let mut x = v;
    while ancestor[ancestor[x as usize] as usize] != NONE {
        path.push(x);
        x = ancestor[x as usize];
    }
    while let Some(y) = path.pop() {
        let a = ancestor[y as usize];
        if semi[label[a as usize] as usize] < semi[label[y as usize] as usize] {
            label[y as usize] = label[a as usize];
        }
        ancestor[y as usize] = ancestor[a as usize];
    }
    label[v as usize]
}
