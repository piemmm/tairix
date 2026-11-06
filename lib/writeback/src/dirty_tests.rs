use super::*;
use tairix_fuzzseed::meter::{metered, refusing_above, refusing_after};

const BS: usize = 512;
const BLOCK: core::num::NonZeroUsize = core::num::NonZeroUsize::new(BS).unwrap();

/// Gather bound wide enough that a test's runs are never window-bounded.
const WIDE: usize = 64 * BS;

fn block(fill: u8) -> [u8; BS] {
    [fill; BS]
}

/// A block whose every byte names the address it is staged at.
fn tagged(phys: u64) -> [u8; BS] {
    block(u8::try_from(phys).expect("a test address fits a byte"))
}

/// Every request the drain issued: start address, block count, and the fill
/// byte of each block it carried.
fn requests(set: &mut DirtySet, phase: WritePhase, run_bytes: usize) -> Vec<(u64, usize, Vec<u8>)> {
    let mut seen = Vec::new();
    set.drain(phase, run_bytes, |phys, run| {
        seen.push((
            phys,
            run.len() / BS,
            run.chunks(BS).map(|block| block[0]).collect(),
        ));
        Ok(())
    })
    .expect("drain");
    seen
}

#[test]
fn a_rewrite_replaces_the_staged_block_rather_than_adding_one() {
    let mut set = DirtySet::new(BLOCK);
    set.stage(WritePhase::BeforeBarrier, 7, &block(0xAA))
        .expect("stage");
    set.stage(WritePhase::BeforeBarrier, 7, &block(0xBB))
        .expect("restage");
    assert_eq!(set.len(), 1);
    assert_eq!(
        requests(&mut set, WritePhase::BeforeBarrier, WIDE),
        alloc::vec![(7, 1, alloc::vec![0xBB])]
    );
}

#[test]
fn moving_a_block_between_phases_replaces_its_single_entry() {
    let mut set = DirtySet::new(BLOCK);
    set.stage(WritePhase::BeforeBarrier, 7, &block(0xAA))
        .expect("stage");
    set.stage(WritePhase::AfterBarrier, 7, &block(0xBB))
        .expect("move");
    assert!(!set.has(WritePhase::BeforeBarrier));
    assert_eq!(set.len(), 1);
    assert_eq!(
        requests(&mut set, WritePhase::AfterBarrier, WIDE),
        alloc::vec![(7, 1, alloc::vec![0xBB])]
    );
}

#[test]
fn draining_one_block_leaves_the_rest_of_its_phase_staged() {
    let mut set = DirtySet::new(BLOCK);
    set.stage(WritePhase::BeforeBarrier, 7, &block(0xAA))
        .expect("stage marker");
    set.stage(WritePhase::BeforeBarrier, 9, &block(0xBB))
        .expect("stage transaction block");
    let mut seen = None;
    set.drain_block(WritePhase::BeforeBarrier, 7, |phys, bytes| {
        seen = Some((phys, bytes[0]));
        Ok(())
    })
    .expect("drain marker");
    assert_eq!(seen, Some((7, 0xAA)));
    assert_eq!(
        requests(&mut set, WritePhase::BeforeBarrier, WIDE),
        alloc::vec![(9, 1, alloc::vec![0xBB])]
    );
}

#[test]
fn clearing_one_phase_preserves_the_other() {
    let mut set = DirtySet::new(BLOCK);
    set.stage(WritePhase::BeforeBarrier, 7, &block(0xAA))
        .expect("stage before");
    set.stage(WritePhase::AfterBarrier, 9, &block(0xBB))
        .expect("stage after");
    set.clear_phase(WritePhase::BeforeBarrier);
    assert!(!set.has(WritePhase::BeforeBarrier));
    assert_eq!(
        requests(&mut set, WritePhase::AfterBarrier, WIDE),
        alloc::vec![(9, 1, alloc::vec![0xBB])]
    );
}

#[test]
fn the_drain_runs_in_ascending_device_order() {
    let mut set = DirtySet::new(BLOCK);
    for (fill, phys) in [(1_u8, 9_u64), (2, 2), (3, 40), (4, 3)] {
        set.stage(WritePhase::BeforeBarrier, phys, &block(fill))
            .expect("stage");
    }
    // 2 and 3 are adjacent and leave together; 9 and 40 stand alone.
    assert_eq!(
        requests(&mut set, WritePhase::BeforeBarrier, WIDE),
        alloc::vec![
            (2, 2, alloc::vec![2, 4]),
            (9, 1, alloc::vec![1]),
            (40, 1, alloc::vec![3]),
        ]
    );
    assert_eq!(set.len(), 0);
}

#[test]
fn adjacent_blocks_leave_as_one_request_and_a_gap_ends_the_run() {
    let mut set = DirtySet::new(BLOCK);
    for phys in [10_u64, 11, 12, 14, 15] {
        set.stage(WritePhase::BeforeBarrier, phys, &tagged(phys))
            .expect("stage");
    }
    assert_eq!(
        requests(&mut set, WritePhase::BeforeBarrier, WIDE),
        alloc::vec![
            (10, 3, alloc::vec![10, 11, 12]),
            (14, 2, alloc::vec![14, 15]),
        ],
        "a run must cover only addresses the set holds"
    );
}

#[test]
fn a_run_is_bounded_by_the_gather_window() {
    let mut set = DirtySet::new(BLOCK);
    for phys in 0..5_u64 {
        set.stage(WritePhase::BeforeBarrier, phys, &tagged(phys))
            .expect("stage");
    }
    // Five adjacent blocks under a two-block bound: 2 + 2 + 1.
    let issued = requests(&mut set, WritePhase::BeforeBarrier, 2 * BS);
    assert_eq!(
        issued
            .iter()
            .map(|&(phys, blocks, _)| (phys, blocks))
            .collect::<Vec<_>>(),
        alloc::vec![(0, 2), (2, 2), (4, 1)]
    );
}

#[test]
fn with_no_gather_window_each_block_is_written_from_the_set() {
    let mut set = DirtySet::new(BLOCK);
    for phys in 0..3_u64 {
        set.stage(WritePhase::BeforeBarrier, phys, &tagged(phys))
            .expect("stage");
    }
    // A bound below one block leaves nothing to gather into, which is the
    // path a refused window allocation takes.
    assert_eq!(
        requests(&mut set, WritePhase::BeforeBarrier, BS - 1),
        alloc::vec![
            (0, 1, alloc::vec![0]),
            (1, 1, alloc::vec![1]),
            (2, 1, alloc::vec![2]),
        ]
    );
    assert_eq!(set.len(), 0);
}

#[test]
fn a_failing_write_leaves_the_blocks_it_did_not_reach_staged() {
    let mut set = DirtySet::new(BLOCK);
    for phys in [0_u64, 2, 4, 6] {
        set.stage(WritePhase::BeforeBarrier, phys, &block(0))
            .expect("stage");
    }
    let mut written = 0_usize;
    let outcome = set.drain(WritePhase::BeforeBarrier, WIDE, |_, _| {
        written += 1;
        if written == 2 {
            return Err(DriverError::DeviceFault);
        }
        Ok(())
    });
    assert_eq!(outcome, Err(DriverError::DeviceFault));
    assert_eq!(written, 2);
    assert_eq!(set.len(), 3, "the failed and untouched blocks stay staged");
}

#[test]
fn the_overlay_replaces_only_the_staged_blocks_of_a_run() {
    let mut set = DirtySet::new(BLOCK);
    set.stage(WritePhase::AfterBarrier, 11, &block(0xCC))
        .expect("stage");
    let mut run = alloc::vec![0x11_u8; 3 * BS];
    set.overlay(10, &mut run).expect("overlay");
    assert_eq!(run[0], 0x11);
    assert_eq!(run[BS], 0xCC);
    assert_eq!(run[2 * BS], 0x11);
    assert_eq!(set.staged_in(10, 3), 1);
    assert_eq!(set.staged_in(12, 3), 0);
}

#[test]
fn a_short_or_ragged_buffer_is_refused_rather_than_truncated() {
    let mut set = DirtySet::new(BLOCK);
    assert_eq!(
        set.stage(WritePhase::BeforeBarrier, 1, &[0u8; BS - 1]),
        Err(DriverError::BufferTooSmall)
    );
    assert_eq!(
        set.overlay(0, &mut [0u8; BS + 1]),
        Err(DriverError::BufferTooSmall)
    );
}

#[test]
fn the_sets_pinned_figure_follows_what_it_holds() {
    let mut set = DirtySet::new(BLOCK);
    assert_eq!((set.pinned_bytes(), set.held()), (0, 0));

    set.stage(WritePhase::BeforeBarrier, 4, &block(1))
        .expect("stage");
    set.stage(WritePhase::BeforeBarrier, 5, &block(2))
        .expect("stage");
    let per_block = BS + MAP_ENTRY_OVERHEAD;
    assert_eq!((set.pinned_bytes(), set.held()), (2 * per_block, 2));

    let _ = requests(&mut set, WritePhase::BeforeBarrier, WIDE);
    assert_eq!(
        (set.pinned_bytes(), set.held()),
        (0, 0),
        "a drained set pins nothing"
    );
}

#[test]
fn a_saved_prior_version_is_pinned_like_a_staged_one() {
    let mut set = DirtySet::new(BLOCK);
    set.begin_operation();
    set.stage(WritePhase::BeforeBarrier, 7, &block(1))
        .expect("stage");
    set.end_operation();

    set.begin_operation();
    set.stage(WritePhase::BeforeBarrier, 7, &block(2))
        .expect("rewrite");
    assert_eq!(
        set.held(),
        2,
        "the prior version and the new one are both held"
    );
    set.end_operation();
    assert_eq!(set.held(), 1, "the undo copy goes with the operation");
}

#[test]
fn undoing_an_operation_restores_what_an_earlier_one_staged() {
    let mut set = DirtySet::new(BLOCK);
    set.begin_operation();
    set.stage(WritePhase::BeforeBarrier, 7, &block(0xAA))
        .expect("first operation stages");
    set.end_operation();

    set.begin_operation();
    set.stage(WritePhase::BeforeBarrier, 7, &block(0xBB))
        .expect("second operation rewrites it");
    set.stage(WritePhase::BeforeBarrier, 9, &block(0xCC))
        .expect("and stages one of its own");
    set.undo_operation().expect("undo");

    assert_eq!(
        requests(&mut set, WritePhase::BeforeBarrier, WIDE),
        alloc::vec![(7, 1, alloc::vec![0xAA])],
        "the earlier operation's block must survive with its own bytes"
    );
}

#[test]
fn undoing_an_operation_restores_a_block_it_discarded_and_took_again() {
    let mut set = DirtySet::new(BLOCK);
    set.begin_operation();
    set.stage(WritePhase::BeforeBarrier, 4, &block(0xAA))
        .expect("first operation stages");
    set.end_operation();

    set.begin_operation();
    set.discard_run(4, 1).expect("discard");
    set.stage(WritePhase::BeforeBarrier, 4, &block(0xBB))
        .expect("reuse the freed block");
    set.undo_operation().expect("undo");

    assert_eq!(
        requests(&mut set, WritePhase::BeforeBarrier, WIDE),
        alloc::vec![(4, 1, alloc::vec![0xAA])]
    );
}

#[test]
fn ending_an_operation_keeps_its_changes() {
    let mut set = DirtySet::new(BLOCK);
    set.begin_operation();
    set.stage(WritePhase::BeforeBarrier, 7, &block(0xAA))
        .expect("stage");
    set.end_operation();
    set.begin_operation();
    set.stage(WritePhase::BeforeBarrier, 7, &block(0xBB))
        .expect("restage");
    set.end_operation();
    set.undo_operation().expect("undo");
    assert_eq!(
        requests(&mut set, WritePhase::BeforeBarrier, WIDE),
        alloc::vec![(7, 1, alloc::vec![0xBB])],
        "an ended operation has nothing left to undo"
    );
}

#[test]
fn undoing_a_whole_restage_brings_back_the_phase_it_replaced() {
    let mut set = DirtySet::new(BLOCK);
    set.begin_operation();
    set.stage(WritePhase::BeforeBarrier, 7, &block(0xAA))
        .expect("an ordered block");
    set.end_operation();

    set.begin_operation();
    set.stage(WritePhase::AfterBarrier, 7, &block(0xBB))
        .expect("restaged whole after the barrier");
    assert_eq!(
        set.held(),
        2,
        "the version it replaced is kept for the undo"
    );
    set.undo_operation().expect("undo");

    assert_eq!(set.held(), 1);
    assert!(requests(&mut set, WritePhase::AfterBarrier, WIDE).is_empty());
    assert_eq!(
        requests(&mut set, WritePhase::BeforeBarrier, WIDE),
        alloc::vec![(7, 1, alloc::vec![0xAA])]
    );
}

#[test]
fn undoing_an_operation_leaves_an_after_barrier_block_staged() {
    let mut set = DirtySet::new(BLOCK);
    set.begin_operation();
    set.stage(WritePhase::AfterBarrier, 12, &block(0xDD))
        .expect("stage a rebuildable block");
    set.undo_operation().expect("undo");
    assert_eq!(
        requests(&mut set, WritePhase::AfterBarrier, WIDE),
        alloc::vec![(12, 1, alloc::vec![0xDD])]
    );
}

#[test]
fn a_savepoint_records_a_block_once_however_often_it_is_rewritten() {
    let mut set = DirtySet::new(BLOCK);
    set.begin_operation();
    set.stage(WritePhase::BeforeBarrier, 7, &block(1))
        .expect("stage");
    assert!(set.operation_changed());
    for fill in 2..8_u8 {
        set.stage(WritePhase::BeforeBarrier, 7, &block(fill))
            .expect("rewrite");
    }
    set.undo_operation().expect("undo");
    assert_eq!(set.len(), 0);
}

#[test]
fn discarding_a_reclaimed_block_drops_it_from_the_drain() {
    let mut set = DirtySet::new(BLOCK);
    set.stage(WritePhase::BeforeBarrier, 4, &block(1))
        .expect("stage");
    set.stage(WritePhase::BeforeBarrier, 5, &block(2))
        .expect("stage");
    set.discard_run(4, 1).expect("discard");
    set.discard_run(99, 1).expect("discard nothing");
    assert_eq!(
        requests(&mut set, WritePhase::BeforeBarrier, WIDE),
        alloc::vec![(5, 1, alloc::vec![2])],
        "a discarded block must not rejoin its neighbour's run"
    );
}

#[test]
fn an_in_place_edit_starts_from_the_device_and_then_from_the_staged_copy() {
    let mut set = DirtySet::new(BLOCK);
    let mut loads = 0;
    let held = set
        .stage_with(WritePhase::BeforeBarrier, 3, |buf| {
            loads += 1;
            buf.fill(0x40);
            Ok(())
        })
        .expect("first touch loads");
    held[0] = 0x41;
    let held = set
        .stage_with(WritePhase::BeforeBarrier, 3, |_| {
            loads += 1;
            Ok(())
        })
        .expect("second touch edits the staged copy");
    held[1] = 0x42;
    assert_eq!(loads, 1, "a staged block is never reloaded from the device");
    let staged = set.staged(3).expect("staged");
    assert_eq!((staged[0], staged[1], staged[2]), (0x41, 0x42, 0x40));
}

#[test]
fn a_failed_load_stages_nothing() {
    let mut set = DirtySet::new(BLOCK);
    set.begin_operation();
    assert_eq!(
        set.stage_with(WritePhase::BeforeBarrier, 3, |_| Err(
            DriverError::DeviceFault
        ))
        .err(),
        Some(DriverError::DeviceFault)
    );
    assert!(set.is_empty());
    assert_eq!(set.staged(3), None);
}

#[test]
fn undoing_an_in_place_edit_restores_the_earlier_operations_bytes() {
    let mut set = DirtySet::new(BLOCK);
    set.begin_operation();
    set.stage(WritePhase::BeforeBarrier, 8, &block(0x10))
        .expect("stage");
    set.end_operation();

    set.begin_operation();
    set.stage_with(WritePhase::BeforeBarrier, 8, |_| Ok(()))
        .expect("edit")[0] = 0x99;
    set.undo_operation().expect("undo");
    assert_eq!(set.staged(8).map(|bytes| bytes[0]), Some(0x10));
}

#[test]
fn the_block_walk_is_ascending_and_leaves_the_set_intact() {
    let mut set = DirtySet::new(BLOCK);
    for phys in [30_u64, 10, 20] {
        set.stage(WritePhase::BeforeBarrier, phys, &tagged(phys))
            .expect("stage");
    }
    set.stage(WritePhase::AfterBarrier, 15, &tagged(15))
        .expect("stage the other phase");
    let walked: Vec<(u64, u8)> = set
        .blocks(WritePhase::BeforeBarrier)
        .map(|(phys, bytes)| (phys, bytes[0]))
        .collect();
    assert_eq!(walked, alloc::vec![(10, 10), (20, 20), (30, 30)]);
    assert_eq!(set.count(WritePhase::BeforeBarrier), 3);
    assert_eq!(set.len(), 4, "walking drains nothing");
}

/// Every block the set stages, in both phases, and the buffers it pins.
fn contents(set: &DirtySet) -> (Vec<(WritePhase, u64, Vec<u8>)>, usize) {
    let mut staged = Vec::new();
    for phase in WritePhase::ALL {
        staged.extend(
            set.blocks(phase)
                .map(|(phys, bytes)| (phase, phys, bytes.to_vec())),
        );
    }
    (staged, set.held())
}

/// The blocks `phase` holds, with their bytes.
fn staged_blocks(set: &DirtySet, phase: WritePhase) -> Vec<(u64, Vec<u8>)> {
    set.blocks(phase)
        .map(|(phys, bytes)| (phys, bytes.to_vec()))
        .collect()
}

/// `0..count` staged by an operation that ended, each block naming its
/// address, with a later operation opened over them.
fn earlier_blocks(count: u64) -> DirtySet {
    let mut set = DirtySet::new(BLOCK);
    set.begin_operation();
    for phys in 0..count {
        set.stage(WritePhase::BeforeBarrier, phys, &tagged(phys))
            .expect("stage");
    }
    set.end_operation();
    set.begin_operation();
    set
}

/// Run `change` with no allocation granted, then one more each time until it
/// goes through, holding each refusal to an `OutOfMemory` that left the set
/// as it was.
fn refused_until_done(
    set: &mut DirtySet,
    mut change: impl FnMut(&mut DirtySet) -> Result<(), DriverError>,
) {
    for granted in 0..32 {
        let before = contents(set);
        match refusing_after(granted, || change(set)) {
            Ok(()) => return,
            Err(err) => {
                assert_eq!(
                    err,
                    DriverError::OutOfMemory,
                    "refused after {granted} grants"
                );
                assert_eq!(contents(set), before, "refused after {granted} grants");
            }
        }
    }
    panic!("the change never went through");
}

#[test]
fn a_block_refused_its_buffer_is_out_of_memory_not_a_full_volume() {
    let mut set = DirtySet::new(BLOCK);
    let refused = refusing_above(0, || set.stage(WritePhase::BeforeBarrier, 1, &block(1)));
    assert_eq!(refused, Err(DriverError::OutOfMemory));
    assert!(set.is_empty());
}

#[test]
fn a_change_the_heap_refuses_leaves_the_set_as_it_was() {
    let mut set = earlier_blocks(24);
    // A fresh block, one an earlier operation staged, an edit in place, a move
    // across the barrier, and a discard the savepoint must take.
    refused_until_done(&mut set, |set| {
        set.stage(WritePhase::BeforeBarrier, 40, &block(1))
    });
    refused_until_done(&mut set, |set| {
        set.stage(WritePhase::BeforeBarrier, 3, &block(2))
    });
    refused_until_done(&mut set, |set| {
        set.stage_with(WritePhase::BeforeBarrier, 5, |_| Ok(()))
            .map(drop)
    });
    refused_until_done(&mut set, |set| {
        set.stage(WritePhase::AfterBarrier, 7, &block(3))
    });
    refused_until_done(&mut set, |set| set.discard_run(10, 6));
    set.undo_operation().expect("undo");
    let earlier: Vec<(u64, Vec<u8>)> = (0..24_u64)
        .map(|phys| (phys, tagged(phys).to_vec()))
        .collect();
    assert_eq!(staged_blocks(&set, WritePhase::BeforeBarrier), earlier);
    assert!(!set.has(WritePhase::AfterBarrier));
}

#[test]
fn undoing_an_operation_asks_the_heap_for_nothing() {
    let mut set = earlier_blocks(64);
    // Rewrites, a move across the barrier, a discard and fresh blocks: an undo
    // that must put back, move back and take out.
    for phys in 0..16_u64 {
        set.stage(WritePhase::BeforeBarrier, phys, &block(0xEE))
            .expect("rewrite");
    }
    set.stage(WritePhase::AfterBarrier, 20, &block(0xDD))
        .expect("move across the barrier");
    set.discard_run(30, 30).expect("discard");
    for phys in 100..140_u64 {
        set.stage(WritePhase::BeforeBarrier, phys, &block(0xCC))
            .expect("stage a fresh block");
    }
    let (undone, metering) = metered(|| refusing_above(0, || set.undo_operation()));
    assert_eq!(undone, Ok(()));
    assert_eq!(metering.allocations, 0, "{metering:?}");
    let earlier: Vec<(u64, Vec<u8>)> = (0..64_u64)
        .map(|phys| (phys, tagged(phys).to_vec()))
        .collect();
    assert_eq!(staged_blocks(&set, WritePhase::BeforeBarrier), earlier);
    assert!(!set.has(WritePhase::AfterBarrier));
}

#[test]
fn a_set_drained_empty_holds_nothing_back() {
    let mut set = DirtySet::new(BLOCK);
    let ((), metering) = metered(|| {
        for phys in 0..200_u64 {
            set.stage(WritePhase::BeforeBarrier, phys, &block(1))
                .expect("stage");
        }
        drop(requests(&mut set, WritePhase::BeforeBarrier, WIDE));
    });
    assert_eq!(metering.retained, 0, "{metering:?}");
}

/// A block staged for the other phase moves its buffer across when no
/// savepoint keeps the version it stands as, rather than being copied and the
/// original dropped.
#[test]
fn an_unsaved_move_across_the_barrier_takes_its_buffer_with_it() {
    let mut set = DirtySet::new(BLOCK);
    set.stage(WritePhase::AfterBarrier, 7, &block(0x5A))
        .expect("stage");
    let (edited, metering) = metered(|| {
        set.stage_with(WritePhase::BeforeBarrier, 7, |_| {
            Err(DriverError::DeviceFault)
        })
        .map(|held| held[0])
    });
    assert_eq!(edited, Ok(0x5A), "the staged bytes are the ones edited");
    assert_eq!(metering.allocations, 0, "{metering:?}");
    assert!(!set.has(WritePhase::AfterBarrier));
    assert_eq!(set.len(), 1);
}

/// The move with an operation running that recorded the block already: its
/// undo still leaves the set as the operation found it.
#[test]
fn undoing_an_unsaved_move_across_the_barrier_restores_the_set() {
    let mut set = DirtySet::new(BLOCK);
    set.begin_operation();
    set.stage(WritePhase::BeforeBarrier, 7, &block(1))
        .expect("stage");
    set.stage(WritePhase::AfterBarrier, 7, &block(2))
        .expect("move across");
    set.stage_with(WritePhase::BeforeBarrier, 7, |_| Ok(()))
        .expect("and back")[0] = 3;
    assert_eq!(set.staged(7).map(|bytes| bytes[0]), Some(3));
    set.undo_operation().expect("undo");
    assert!(
        set.is_empty(),
        "the operation staged the block, so its undo takes it out"
    );
}
