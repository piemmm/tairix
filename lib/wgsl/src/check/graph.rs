//! Directed graphs over a module's declarations — what each header names,
//! which functions each calls, what each entry point reaches — each node's
//! successors a run of one list, ordered and walked without recursion.

use alloc::vec::Vec;

use crate::diag::{push, reserve, Outcome, Stop};

/// A directed graph of numbered nodes, each node's successors a run of
/// `edges`.
#[derive(Default)]
pub(crate) struct Graph {
    starts: Vec<u32>,
    edges: Vec<u32>,
}

impl Graph {
    /// The graph of `count` nodes whose edges `pairs` lists as `(from, to)`
    /// in any order, a repeated edge kept once.
    pub(crate) fn from_edges(count: usize, pairs: &mut [(u32, u32)]) -> Outcome<Self> {
        pairs.sort_unstable();
        let mut starts = Vec::new();
        let mut edges = Vec::new();
        reserve(&mut starts, count + 1)?;
        reserve(&mut edges, pairs.len())?;
        let mut at = 0usize;
        for node in 0..count {
            let node = u32::try_from(node).map_err(|_| Stop::OutOfMemory)?;
            starts.push(u32::try_from(edges.len()).map_err(|_| Stop::OutOfMemory)?);
            let mut previous = None;
            while let Some(&(from, to)) = pairs.get(at) {
                if from != node {
                    break;
                }
                if previous != Some(to) && (to as usize) < count {
                    edges.push(to);
                    previous = Some(to);
                }
                at += 1;
            }
        }
        starts.push(u32::try_from(edges.len()).map_err(|_| Stop::OutOfMemory)?);
        Ok(Self { starts, edges })
    }

    /// How many nodes it has.
    pub(crate) fn len(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }

    /// The nodes `node` has an edge to.
    pub(crate) fn successors(&self, node: u32) -> &[u32] {
        let range = self.starts.get(node as usize).copied().unwrap_or(0) as usize
            ..self.starts.get(node as usize + 1).copied().unwrap_or(0) as usize;
        self.edges.get(range).unwrap_or(&[])
    }

    /// Every node, each after every node it reaches; or, where a cycle
    /// makes that impossible, a node on the cycle.
    pub(crate) fn order(&self) -> Outcome<Result<Vec<u32>, u32>> {
        let count = self.len();
        // 0 unvisited, 1 on the walk, 2 done.
        let mut state: Vec<u8> = Vec::new();
        reserve(&mut state, count)?;
        state.resize(count, 0);
        let mut order = Vec::new();
        reserve(&mut order, count)?;
        let mut stack: Vec<(u32, usize)> = Vec::new();
        for start in 0..count {
            if state[start] != 0 {
                continue;
            }
            let start = u32::try_from(start).map_err(|_| Stop::OutOfMemory)?;
            state[start as usize] = 1;
            push(&mut stack, (start, 0))?;
            while let Some(&mut (node, ref mut next)) = stack.last_mut() {
                if let Some(&target) = self.successors(node).get(*next) {
                    *next += 1;
                    match state[target as usize] {
                        0 => {
                            state[target as usize] = 1;
                            push(&mut stack, (target, 0))?;
                        }
                        1 => return Ok(Err(target)),
                        _ => {}
                    }
                    continue;
                }
                state[node as usize] = 2;
                order.push(node);
                stack.pop();
            }
        }
        Ok(Ok(order))
    }

    /// Mark in `seen` every node `from` reaches, itself included.
    pub(crate) fn reach(&self, from: u32, seen: &mut [bool], stack: &mut Vec<u32>) -> Outcome<()> {
        stack.clear();
        if let Some(mark) = seen.get_mut(from as usize) {
            if !*mark {
                *mark = true;
                push(stack, from)?;
            }
        }
        while let Some(node) = stack.pop() {
            for &target in self.successors(node) {
                if let Some(mark) = seen.get_mut(target as usize) {
                    if !*mark {
                        *mark = true;
                        push(stack, target)?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Graph;

    #[test]
    fn orders_dependencies_first_and_finds_a_cycle() {
        let mut pairs = [(0, 1), (1, 2), (0, 2), (0, 1)];
        let graph = Graph::from_edges(3, &mut pairs).expect("built");
        assert_eq!(graph.successors(0), &[1, 2], "a repeated edge is kept once");
        assert_eq!(graph.order().expect("ordered"), Ok(alloc::vec![2, 1, 0]));
        let mut seen = [false; 3];
        graph
            .reach(1, &mut seen, &mut alloc::vec::Vec::new())
            .expect("walked");
        assert_eq!(seen, [false, true, true]);
        let mut cyclic = [(0, 1), (1, 2), (2, 1)];
        let graph = Graph::from_edges(3, &mut cyclic).expect("built");
        assert!(matches!(graph.order().expect("ordered"), Err(1 | 2)));
    }

    #[test]
    fn a_long_chain_orders_without_recursion() {
        let mut pairs: alloc::vec::Vec<(u32, u32)> = (0..100_000).map(|n| (n, n + 1)).collect();
        let graph = Graph::from_edges(100_001, &mut pairs).expect("built");
        let order = graph.order().expect("ordered").expect("acyclic");
        assert_eq!(order.first(), Some(&100_000));
        assert_eq!(order.last(), Some(&0));
    }
}
