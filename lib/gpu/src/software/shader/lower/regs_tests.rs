use alloc::vec::Vec;

use super::super::layout::{Built, Laid, Term};
use super::super::VIRTUAL;
use super::{
    assign, physical, positions, rebuild, widen, Copied, Fault, Pool, Range, MAX_WIDENING_STEPS,
    SMALL,
};
use crate::software::shader::ir::{Inst, Reg};

#[test]
fn a_freed_slot_is_reused_only_for_a_value_of_its_size() {
    let mut pool = Pool::new(0);
    let small = pool.take(4).expect("a slot");
    let large = pool.take(100).expect("a slot");
    let larger = pool.take(200).expect("a slot");
    for (slot, words) in [(small, 4), (large, 100), (larger, 200)] {
        pool.free(slot, words).expect("freed");
    }
    assert_eq!(pool.take(200), Ok(larger));
    assert_eq!(pool.take(100), Ok(large));
    assert_eq!(
        pool.take(100),
        Ok(304),
        "none of that size is left, so a new one"
    );
    assert_eq!(pool.take(4), Ok(small));
}

#[test]
fn slots_of_many_sizes_are_each_found_by_their_size() {
    let mut pool = Pool::new(0);
    let first = u32::try_from(SMALL).expect("a few");
    let taken: Vec<(u32, u32)> = (first..first + 5000)
        .map(|words| (words, pool.take(words).expect("a slot")))
        .collect();
    for &(words, slot) in &taken {
        pool.free(slot, words).expect("freed");
    }
    let top = pool.top;
    for &(words, slot) in taken.iter().rev() {
        assert_eq!(pool.take(words), Ok(slot), "{words} words");
    }
    assert_eq!(pool.top, top, "every one reused");
}

/// A layout of `depth` loops nested one in the next between a block before
/// them all and one after: loop `i` spans blocks `1 + i` to `2 * depth - i`.
fn nest(depth: u32) -> (Laid, Vec<u32>) {
    let blocks = 2 * depth + 2;
    let laid = Laid {
        blocks: (0..blocks)
            .map(|_| Built {
                insts: Vec::new(),
                term: Term::Exit,
            })
            .collect(),
        places: Vec::new(),
        loops: (0..depth).map(|i| (1 + i, 2 * depth - i)).collect(),
    };
    let starts = positions(&laid).expect("positions");
    (laid, starts)
}

#[test]
fn ranges_widen_over_every_loop_a_lane_can_run_again_while_another_holds_them() {
    let depth = 300;
    let (laid, starts) = nest(depth);
    let after = 2 * depth + 1;
    let deepest = depth;
    let range = |start, end| Some(Range { start, end });
    // Each virtual register's class: the last two share the tile's slots.
    let virtuals = [
        (0, 1, false),
        (1, 1, false),
        (2, 1, false),
        (3, 1, true),
        (4, 1, true),
    ];
    let mut ranges = [
        range(0, deepest),
        range(after, after),
        range(deepest, after),
        range(deepest, after),
        range(deepest, 2 * depth),
    ];
    widen(&mut ranges, &virtuals, &laid, &starts).expect("widened");
    let spans: Vec<(u32, u32)> = ranges.iter().flatten().map(|r| (r.start, r.end)).collect();
    assert_eq!(
        spans,
        [
            (0, 2 * depth),
            (after, after),
            (deepest, after),
            (1, after),
            (2, 2 * depth),
        ],
        "into every loop it reaches; past the nest; a lane's own value left \
         past its loops; a shared one back to the outermost loop it leaves, \
         and to the second where the first holds its last use"
    );
}

#[test]
fn widening_past_its_steps_is_refused() {
    let depth = 1000;
    let (laid, starts) = nest(depth);
    let count = MAX_WIDENING_STEPS / depth as usize + 1;
    let virtuals: Vec<(u32, u32, bool)> = (0..count)
        .map(|i| (u32::try_from(i).expect("few"), 1, false))
        .collect();
    let mut ranges: Vec<Option<Range>> = (0..count)
        .map(|_| {
            Some(Range {
                start: 0,
                end: depth,
            })
        })
        .collect();
    assert_eq!(
        widen(&mut ranges, &virtuals, &laid, &starts),
        Err(Fault::Budget)
    );
}

/// Four registers of four varying words: one live throughout, a copy's
/// destination numbered before its source, the source, and one that starts
/// after the copy ends; their slots, and how many copies were handed theirs.
fn copied_registers(source_start: u32) -> (Vec<u32>, usize) {
    let virtuals = [(0, 4, false), (4, 4, false), (8, 4, false), (12, 4, false)];
    let range = |start, end| Some(Range { start, end });
    let ranges = [
        range(0, 100),
        range(10, 20),
        range(source_start, 10),
        range(30, 40),
    ];
    let copies = [Copied {
        dst: 1,
        at: 10,
        src: 2,
    }];
    let assigned = assign(&virtuals, 0, &ranges, &copies).expect("assigned");
    (assigned.slots, assigned.handed)
}

#[test]
fn a_copy_takes_its_source_s_slot_only_where_the_source_held_one_first() {
    let (slots, handed) = copied_registers(5);
    assert_eq!(slots[1], slots[2], "the source's slot, handed on");
    assert_eq!(handed, 1, "and counted, as the program drops the copy");
    assert_ne!(slots[1], slots[0]);
    assert_ne!(slots[3], slots[0], "never the slot still live throughout");
    // A source first named at the copy, as a call result no return writes,
    // was never written, so nothing is handed on, however ties are ordered.
    let (slots, handed) = copied_registers(10);
    assert_eq!(handed, 0, "the copy kept");
    assert_ne!(slots[1], slots[0]);
    assert_ne!(slots[3], slots[0]);
}

/// The copy a hand-over makes nothing of is dropped, and the program's table
/// is reserved without it, so no slack is left for the trim to copy away.
#[test]
fn a_copy_handing_its_slot_on_leaves_no_room_behind_it() {
    let virtuals = [(0, 4, false), (4, 4, false)];
    let ranges = [
        Some(Range { start: 0, end: 1 }),
        Some(Range { start: 1, end: 1 }),
    ];
    let copies = [Copied {
        dst: 1,
        at: 1,
        src: 0,
    }];
    let assigned = assign(&virtuals, 0, &ranges, &copies).expect("assigned");
    assert_eq!(assigned.handed, 1);
    let (source, copy) = (Reg::varying(VIRTUAL), Reg::varying(VIRTUAL + 4));
    let laid = Laid {
        blocks: alloc::vec![Built {
            insts: alloc::vec![
                Inst::Zero {
                    dst: source,
                    words: 4,
                },
                Inst::Copy {
                    dst: copy,
                    src: source,
                    words: 4,
                },
            ],
            term: Term::Exit,
        }],
        places: Vec::new(),
        loops: Vec::new(),
    };
    let map = |reg| physical(&virtuals, &assigned, reg);
    let rebuilt = rebuild(laid, (&mut [], &mut []), &map, assigned.handed).expect("rebuilt");
    assert_eq!(rebuilt.insts.len(), 1, "the copy dropped");
    assert_eq!(rebuilt.insts.capacity(), 1, "and no room kept for it");
}
