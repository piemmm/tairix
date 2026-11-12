use alloc::vec::Vec;

use super::*;

/// What each place holds, by place.
#[derive(Clone, Debug, PartialEq, Eq)]
struct State(Vec<(Loc, u32)>);

impl State {
    fn get(&self, loc: Loc) -> u32 {
        self.0
            .iter()
            .find(|&&(at, _)| at == loc)
            .map_or(0, |&(_, v)| v)
    }

    fn set(&mut self, loc: Loc, value: u32) {
        match self.0.iter_mut().find(|(at, _)| *at == loc) {
            Some(held) => held.1 = value,
            None => self.0.push((loc, value)),
        }
    }
}

const TEMP: Loc = Loc::Reg(99);

/// `moves` sequenced, then made one by one, leave every destination holding
/// what its source held before, and every other place but the temporary as
/// it was.
fn check(moves: &[Move]) {
    let mut state = State(Vec::new());
    for (n, m) in (1u32..).zip(moves) {
        state.set(m.from, n * 10);
        if state.get(m.to) == 0 {
            state.set(m.to, 1000 + n);
        }
    }
    state.set(TEMP, 7777);
    let before = state.clone();
    let steps = sequence(moves, TEMP);
    for step in &steps {
        let value = state.get(step.from);
        state.set(step.to, value);
    }
    for m in moves {
        assert_eq!(
            state.get(m.to),
            before.get(m.from),
            "{moves:?} as {steps:?}"
        );
    }
    for &(loc, value) in &before.0 {
        if loc != TEMP && moves.iter().all(|m| m.to != loc) {
            assert_eq!(state.get(loc), value, "{loc:?} untouched by {moves:?}");
        }
    }
    let cycles = steps.len() - moves.iter().filter(|m| m.from != m.to).count();
    assert!(
        cycles <= moves.len() / 2,
        "one save a cycle at most: {steps:?}"
    );
}

const fn mv(from: Loc, to: Loc) -> Move {
    Move { from, to }
}

#[test]
fn cycles_chains_and_fans_are_kept() {
    use Loc::{Reg as R, Slot as S};
    check(&[mv(R(1), R(2)), mv(R(2), R(1))]);
    check(&[mv(R(1), R(2)), mv(R(2), R(3)), mv(R(3), R(1))]);
    check(&[mv(R(1), S(0)), mv(S(0), R(1)), mv(R(2), R(3))]);
    check(&[mv(R(1), R(1)), mv(R(2), R(3))]);
    check(&[mv(R(1), R(2)), mv(R(1), R(3)), mv(R(2), R(4))]);
    check(&[
        mv(R(1), R(2)),
        mv(R(2), R(1)),
        mv(R(3), R(4)),
        mv(R(4), R(3)),
    ]);
    check(&[]);
}

#[test]
fn every_permutation_of_four_places_is_kept() {
    let places = [Loc::Reg(1), Loc::Reg(2), Loc::Slot(0), Loc::Slot(1)];
    let mut order = [0usize, 1, 2, 3];
    // Every permutation, by Heap's algorithm.
    let mut c = [0usize; 4];
    let mut all = Vec::from([order]);
    let mut i = 1;
    while i < 4 {
        if c[i] < i {
            if i % 2 == 0 {
                order.swap(0, i);
            } else {
                order.swap(c[i], i);
            }
            all.push(order);
            c[i] += 1;
            i = 1;
        } else {
            c[i] = 0;
            i += 1;
        }
    }
    assert_eq!(all.len(), 24);
    for perm in all {
        let moves: Vec<Move> = (0..4).map(|n| mv(places[n], places[perm[n]])).collect();
        check(&moves);
    }
}
