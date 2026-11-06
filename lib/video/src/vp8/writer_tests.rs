//! Bitstream writer tests: what a decision costs, every tree's leaves read
//! back from their paths, a block's tokens read back, counted and priced, and
//! a partition refused its room.

use alloc::vec;
use alloc::vec::Vec;

use tairix_fuzzseed::meter::refusing_after;
use tairix_fuzzseed::Prng;

use super::{
    code_block, cost, write_block, write_path, write_tree, Coded, Cost, Counted, Path, PlaneCounts,
    Priced, TokenSink, Writer, DEFAULT_TOKEN_COSTS, TOKEN_PATHS,
};
use crate::vp8::{
    read_block, Bool, BLOCK_COEFFS, BMODE_TREE, COEFF_TREE, DEFAULT_COEFF_PROBS, KF_YMODE_TREE,
    MV_REF_TREE, SMALL_MV_TREE, SPLIT_TREE, SUB_MV_REF_TREE, UV_MODE_TREE, YMODE_TREE, ZIGZAG,
};

/// Every tree the format reads, with how many leaves it has.
const TREES: [(&[i8], u8); 9] = [
    (&COEFF_TREE, 12),
    (&BMODE_TREE, 10),
    (&KF_YMODE_TREE, 5),
    (&YMODE_TREE, 5),
    (&UV_MODE_TREE, 4),
    (&MV_REF_TREE, 5),
    (&SPLIT_TREE, 4),
    (&SUB_MV_REF_TREE, 4),
    (&SMALL_MV_TREE, 8),
];

#[test]
fn a_decision_costs_the_bits_its_odds_leave() {
    assert_eq!((cost(128, false), cost(128, true)), (256, 256));
    assert_eq!((cost(1, false), cost(255, true)), (2048, 2048));
    assert!(cost(255, false) <= 2 && cost(1, true) <= 2);
    for probability in 1..u8::MAX {
        assert!(
            cost(probability + 1, false) <= cost(probability, false),
            "{probability}"
        );
        assert!(
            cost(probability + 1, true) >= cost(probability, true),
            "{probability}"
        );
    }
}

#[test]
fn every_leaf_of_every_tree_reads_back_from_its_path() {
    let mut rng = Prng::new(0x5EED_3700);
    for (tree, leaves) in TREES {
        let probs: Vec<u8> = (0..tree.len() / 2).map(|_| rng.next_u8().max(1)).collect();
        let mut writer = Writer::new();
        for leaf in 0..leaves {
            write_tree(&mut writer, tree, &probs, usize::from(leaf));
        }
        let bytes = writer.close().expect("room for the partition");
        let mut reader = Bool::new(&bytes);
        for leaf in 0..leaves {
            assert_eq!(
                reader.tree(tree, &probs, 0),
                leaf,
                "leaf {leaf} of {tree:?}"
            );
        }
        assert!(!reader.exhausted());
    }
}

#[test]
fn a_path_costs_what_writing_it_costs() {
    let mut rng = Prng::new(0x5EED_3701);
    for (tree, leaves) in TREES {
        let probs: Vec<u8> = (0..tree.len() / 2).map(|_| rng.next_u8()).collect();
        for leaf in 0..leaves {
            let path = Path::to(tree, 0, leaf);
            assert_ne!(path, Path::EMPTY, "leaf {leaf} of {tree:?}");
            let mut spent = Cost::default();
            write_path(&mut spent, &path, &probs);
            assert_eq!(path.cost(&probs), spent.0, "leaf {leaf} of {tree:?}");
        }
        assert_eq!(
            Path::to(tree, 0, leaves),
            Path::EMPTY,
            "past the leaves of {tree:?}"
        );
    }
}

/// The tokens a block codes, kept in order with where each is coded.
#[derive(Default)]
struct Recorded(Vec<((usize, usize, usize), u8)>);

impl TokenSink for Recorded {
    fn token(&mut self, at: (usize, usize, usize), value: u8) {
        self.0.push((at, value));
    }

    fn fixed(&mut self, _: u8, _: bool) {}
}

/// A block of levels from position `first` drawn from `rng`: a few, of
/// magnitudes reaching every token category and the largest the tokens code.
fn drawn_levels(rng: &mut Prng, first: usize) -> [i16; BLOCK_COEFFS] {
    const MAGNITUDES: [i16; 12] = [1, 2, 3, 4, 5, 7, 11, 19, 35, 67, 300, 2114];
    let mut levels = [0; BLOCK_COEFFS];
    for level in levels.iter_mut().skip(first) {
        if rng.below(3) == 0 {
            let magnitude = *rng.pick(&MAGNITUDES);
            *level = if rng.next_u64() & 1 == 0 {
                magnitude
            } else {
                -magnitude
            };
        }
    }
    levels
}

#[test]
fn a_block_reads_back_as_written_and_counts_the_branches_it_takes() {
    let mut rng = Prng::new(0x5EED_3702);
    for _ in 0..200 {
        let (plane, context, first) = (rng.below(4), rng.below(3), rng.below(2));
        let levels = drawn_levels(&mut rng, first);
        let mut writer = Writer::new();
        let coded = write_block(
            &mut writer,
            &DEFAULT_COEFF_PROBS[plane],
            context,
            first,
            &levels,
        );
        let bytes = writer.close().expect("room for the block");
        let mut reader = Bool::new(&bytes);
        let mut coeffs = [0i16; BLOCK_COEFFS];
        let probs = &DEFAULT_COEFF_PROBS;
        let read = read_block(
            &mut reader,
            probs,
            plane,
            context,
            first,
            (1, 1),
            &mut coeffs,
        );
        assert_eq!(read, coded);
        assert!(!reader.exhausted());
        for (scan, &position) in ZIGZAG.iter().enumerate() {
            let expected = if scan < first { 0 } else { levels[scan] };
            assert_eq!(
                coeffs[usize::from(position)],
                expected,
                "scan position {scan}"
            );
        }
        let mut recorded = Recorded::default();
        code_block(&mut recorded, context, first, &levels);
        let mut counts: PlaneCounts = [[[[0; 2]; 11]; 3]; 8];
        code_block(&mut Counted(&mut counts), context, first, &levels);
        let mut expected: PlaneCounts = [[[[0; 2]; 11]; 3]; 8];
        for &((band, at, start), value) in &recorded.0 {
            let steps = TOKEN_PATHS[usize::from(value)].steps();
            for &(node, branch) in steps.iter().skip_while(|&&(node, _)| node != start) {
                expected[band][at][node >> 1][usize::from(branch)] += 1;
            }
        }
        assert_eq!(counts, expected);
    }
}

#[test]
fn a_block_is_priced_from_the_table_as_writing_it_costs() {
    let mut rng = Prng::new(0x5EED_3703);
    for _ in 0..400 {
        let (plane, context, first) = (rng.below(4), rng.below(3), rng.below(2));
        let levels = drawn_levels(&mut rng, first);
        let mut spent = Cost::default();
        let mut coded = Coded {
            sink: &mut spent,
            probs: &DEFAULT_COEFF_PROBS[plane],
        };
        let written = code_block(&mut coded, context, first, &levels);
        let mut priced = Priced {
            costs: &DEFAULT_TOKEN_COSTS[plane],
            spent: 0,
        };
        assert_eq!(code_block(&mut priced, context, first, &levels), written);
        assert_eq!(
            priced.spent, spent.0,
            "plane {plane}, context {context}: {levels:?}"
        );
    }
}

#[test]
fn a_partition_refused_its_room_is_handed_back_as_it_began() {
    let begun = vec![1u8, 2, 3];
    let closed = refusing_after(0, || {
        let mut writer = Writer::onto(begun);
        for _ in 0..64 {
            writer.bit(1, true);
        }
        writer.close()
    });
    assert_eq!(closed, Err(vec![1, 2, 3]));
    let mut writer = Writer::onto(vec![9]);
    writer.literal(0x5A5A, 16);
    let bytes = writer.close().expect("room for the partition");
    assert_eq!(bytes[0], 9, "the bytes before the partition are left be");
    assert_eq!(Bool::new(&bytes[1..]).literal(16), 0x5A5A);
}
