//! The construct ladder: its ancestor and common-ancestor queries agree with
//! a walk up the parents on every shape of tree, as deep as nesting may go.

use alloc::vec::Vec;

use super::{ancestor_at, common, ladder, nesting, Construct, Kind, NONE};
use crate::MAX_NESTING;

/// A tree of constructs, each opened inside the one `parent` picks among
/// those before it or outside every one.
fn tree(count: usize, mut parent: impl FnMut(usize) -> Option<usize>) -> Vec<Construct> {
    let mut list: Vec<Construct> = Vec::new();
    for n in 0..count {
        let up = parent(n).map_or(NONE, |p| u32::try_from(p).unwrap_or(NONE));
        let construct = Construct {
            kind: Kind::Selection,
            header: 0,
            merge: 0,
            other: NONE,
            back: NONE,
            parent: up,
            depth: nesting(&list, up) + 1,
            jump: ladder(&list, up),
            near_loop: NONE,
            near_switch: NONE,
            near_continue: NONE,
        };
        list.push(construct);
    }
    list
}

fn walk_up(list: &[Construct], mut c: u32, at: u32) -> u32 {
    while nesting(list, c) > at {
        c = list[c as usize].parent;
    }
    c
}

fn walk_common(list: &[Construct], mut a: u32, mut b: u32) -> u32 {
    while nesting(list, a) > nesting(list, b) {
        a = list[a as usize].parent;
    }
    while nesting(list, b) > nesting(list, a) {
        b = list[b as usize].parent;
    }
    while a != b {
        a = list[a as usize].parent;
        b = list[b as usize].parent;
    }
    a
}

/// A small deterministic generator, so a failure names its tree.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

fn check(list: &[Construct], rng: &mut Lcg, queries: usize) {
    let count = list.len() + 1;
    let pick = |rng: &mut Lcg| {
        let n = rng.below(count);
        if n == list.len() {
            NONE
        } else {
            u32::try_from(n).unwrap_or(NONE)
        }
    };
    for _ in 0..queries {
        let (a, b) = (pick(rng), pick(rng));
        assert_eq!(
            common(list, a, b),
            walk_common(list, a, b),
            "common({a}, {b})"
        );
        let at = u32::try_from(rng.below(nesting(list, a) as usize + 1)).unwrap_or(0);
        assert_eq!(
            ancestor_at(list, a, at),
            walk_up(list, a, at),
            "ancestor_at({a}, {at})"
        );
    }
}

#[test]
fn the_ladder_agrees_with_the_parents_on_random_trees() {
    let mut rng = Lcg(1);
    for trial in 0..if cfg!(miri) { 4 } else { 200 } {
        let count = 1 + rng.below(200);
        let mut shape = Lcg(trial);
        let list = tree(count, |n| {
            let p = shape.below(n + 1);
            (p < n).then_some(p)
        });
        check(&list, &mut rng, 200);
    }
}

#[test]
fn the_ladder_agrees_with_the_parents_on_a_chain_as_deep_as_nesting_goes() {
    let chain = tree(MAX_NESTING, |n| n.checked_sub(1));
    assert_eq!(
        nesting(&chain, u32::try_from(MAX_NESTING - 1).unwrap_or(NONE)),
        u32::try_from(MAX_NESTING).unwrap_or(0)
    );
    check(&chain, &mut Lcg(7), 2_000);
    // A comb: siblings hung at every depth of a spine.
    let comb = tree(2 * MAX_NESTING, |n| {
        if n < MAX_NESTING {
            n.checked_sub(1)
        } else {
            Some(n - MAX_NESTING)
        }
    });
    check(&comb, &mut Lcg(9), 2_000);
}
