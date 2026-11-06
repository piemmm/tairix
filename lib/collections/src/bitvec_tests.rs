extern crate std;

use alloc::vec::Vec;

use tairix_fuzzseed::Prng;

use super::*;

/// Lengths on either side of a word's and of a summary level's edges.
const LENGTHS: [usize; 13] = [0, 1, 2, 63, 64, 65, 127, 128, 129, 200, 4095, 4096, 4097];

/// The lengths a model run draws from: under Miri, which interprets every
/// model scan, those a word or two long; the towers past one level are
/// interpreted through the cost gate and the fuzz sweep instead.
const MODEL_LENGTHS: &[usize] = if cfg!(miri) {
    LENGTHS.split_at(10).0
} else {
    &LENGTHS
};

/// Model runs, and operations a run, at a cost Miri can interpret.
const MODEL_SEEDS: u64 = if cfg!(miri) { 8 } else { 40 };
const MODEL_STEPS: usize = if cfg!(miri) { 12 } else { 24 };

fn model_next(model: &[bool], from: usize, value: bool) -> Option<usize> {
    (from..model.len()).find(|&at| model[at] == value)
}

fn model_prev(model: &[bool], before: usize, value: bool) -> Option<usize> {
    (0..before.min(model.len()))
        .rev()
        .find(|&at| model[at] == value)
}

/// The positions an agreement is checked at: every one of a short bitmap,
/// and the edges of every word of a long one.
fn probes(len: usize) -> Vec<usize> {
    if len <= 300 {
        return (0..=len + 1).collect();
    }
    (0..=len.div_ceil(WORD))
        .flat_map(|word| {
            [
                word * WORD,
                word * WORD + 1,
                (word * WORD).saturating_sub(1),
            ]
        })
        .chain([len, len + 1])
        .collect()
}

/// Every answer `bits` gives agrees with `model`, and both summaries are what
/// a rebuild from its words makes them.
fn agrees(bits: &BitVec, model: &[bool]) {
    assert_eq!(bits.len(), model.len());
    assert_eq!(bits.count_ones(), model.iter().filter(|&&bit| bit).count());
    assert_eq!(
        bits.count_zeros(),
        model.iter().filter(|&&bit| !bit).count()
    );
    let ones: Vec<usize> = bits.iter_ones().collect();
    let want: Vec<usize> = (0..model.len()).filter(|&at| model[at]).collect();
    assert_eq!(ones, want, "the walk");
    for at in probes(model.len()) {
        assert_eq!(bits.get(at), model.get(at).copied(), "get {at}");
        assert_eq!(
            bits.next_one(at),
            model_next(model, at, true),
            "next_one {at}"
        );
        assert_eq!(
            bits.next_zero(at),
            model_next(model, at, false),
            "next_zero {at}"
        );
        assert_eq!(
            bits.prev_one(at),
            model_prev(model, at, true),
            "prev_one {at}"
        );
        assert_eq!(
            bits.prev_zero(at),
            model_prev(model, at, false),
            "prev_zero {at}"
        );
    }
    let mut rebuilt = bits.try_clone().expect("copies");
    rebuilt.rebuild();
    assert_eq!(rebuilt.set.levels, bits.set.levels, "the set summary");
    assert_eq!(rebuilt.clear.levels, bits.clear.levels, "the clear summary");
}

#[test]
fn every_answer_agrees_with_a_plain_vector() {
    for seed in 0..MODEL_SEEDS {
        let mut prng = Prng::new(seed);
        let len = MODEL_LENGTHS[prng.below(MODEL_LENGTHS.len())];
        let value = prng.below(2) == 1;
        let mut bits = BitVec::try_with_len(len, value).expect("allocates");
        let mut model = alloc::vec![value; len];
        agrees(&bits, &model);
        for _ in 0..MODEL_STEPS {
            let len = model.len();
            match prng.below(5) {
                0 | 1 => {
                    let at = prng.below(len + 2);
                    let value = prng.below(2) == 1;
                    let was = model.get(at).copied();
                    assert_eq!(bits.set(at, value), was, "set {at}");
                    if let Some(slot) = model.get_mut(at) {
                        *slot = value;
                    }
                }
                2 => {
                    let start = prng.below(len + 2);
                    let end = start + prng.below(len + 2);
                    let value = prng.below(2) == 1;
                    let within = end <= len;
                    assert_eq!(bits.set_range(start..end, value), within);
                    if within {
                        model[start..end].fill(value);
                    }
                }
                3 => {
                    let value = prng.below(2) == 1;
                    bits.fill(value);
                    model.fill(value);
                }
                _ => {
                    let len = MODEL_LENGTHS[prng.below(MODEL_LENGTHS.len())];
                    let value = prng.below(2) == 1;
                    bits.try_resize(len, value).expect("allocates");
                    model.resize(len, value);
                }
            }
            agrees(&bits, &model);
        }
    }
}

/// Growing keeps every bit already held, however the old length meets a word.
#[test]
fn a_growth_keeps_what_it_held_and_sets_what_it_gains() {
    for old in [63, 64, 65, 128] {
        let mut bits = BitVec::try_with_len(old, false).expect("allocates");
        bits.set(0, true);
        bits.try_resize(old + 70, true).expect("allocates");
        let mut model = alloc::vec![false; old];
        model[0] = true;
        model.resize(old + 70, true);
        agrees(&bits, &model);
    }
}

#[test]
fn nothing_past_the_end_is_read_or_written() {
    let mut bits = BitVec::try_with_len(70, false).expect("allocates");
    assert_eq!(bits.set(70, true), None);
    assert!(!bits.set_range(60..71, true));
    let reversed = core::ops::Range { start: 5, end: 4 };
    assert!(
        !bits.set_range(reversed, true),
        "a reversed range is refused"
    );
    assert_eq!(bits.count_ones(), 0, "a refused range sets nothing");
    bits.fill(true);
    assert_eq!(
        bits.next_zero(0),
        None,
        "the word's tail is not a clear bit"
    );
    assert_eq!(bits.prev_zero(usize::MAX), None);
    assert_eq!(bits.get(usize::MAX), None);
    assert_eq!(bits.next_one(usize::MAX), None);
}

/// A search over a sparse bitmap reads a handful of summary words, not one
/// per word of bits.
#[test]
fn a_search_costs_the_towers_height_however_sparse() {
    let len = 1 << 20;
    let mut bits = BitVec::try_with_len(len, false).expect("allocates");
    let levels = Spans::of(len / WORD).count;
    assert_eq!(levels, 3);
    for (set, from, before) in [(len - 1, 0, len), (0, 1, len)] {
        bits.fill(false);
        bits.set(set, true);
        bits.set.reads.set(0);
        if set == 0 {
            assert_eq!(bits.prev_one(before), Some(0));
        } else {
            assert_eq!(bits.next_one(from), Some(set));
        }
        assert!(
            bits.set.reads.get() <= 2 * levels,
            "{}",
            bits.set.reads.get()
        );
    }
    bits.fill(true);
    bits.set(len / 2, false);
    bits.clear.reads.set(0);
    assert_eq!(bits.next_zero(0), Some(len / 2));
    assert!(bits.clear.reads.get() <= 2 * levels);
}

#[test]
fn a_copy_is_equal_and_independent() {
    let mut bits = BitVec::try_with_len(130, false).expect("allocates");
    bits.set_range(10..90, true);
    let mut copy = bits.try_clone().expect("copies");
    assert_eq!(copy, bits);
    copy.set(5, true);
    assert_ne!(copy, bits);
    assert_eq!(bits.get(5), Some(false));
}

#[test]
fn a_reservation_covers_the_growth_it_names() {
    let mut bits = BitVec::new();
    bits.try_reserve(5000).expect("allocates");
    let held = bits.held_bytes();
    bits.try_resize(5000, true).expect("within the reservation");
    assert_eq!(bits.held_bytes(), held, "the growth allocated");
    assert_eq!(
        bits.try_reserve(usize::MAX),
        Err(TryReserveError::CapacityOverflow)
    );
}
