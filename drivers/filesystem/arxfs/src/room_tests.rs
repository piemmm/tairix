//! Memory refused to the allocator's run sets: a step refused changes nothing,
//! the undo paths that cannot fail never ask the heap at all, a volume an
//! operation was refused on is the volume it was, and a commit refused keeps
//! the batch it would have published. The room the sets keep for those paths
//! is made a batch of steps at a time, pinned with the runs, and given back
//! once its transaction is published.

use core::ops::Range;

use super::*;
use crate::reserve::Draw;
use tairix_fuzzseed::meter::{metered, refusing_above, refusing_after};
use tairix_writeback::writeback_window_ns;

/// Every run set the allocator holds, the free count, and the blocks the map
/// marks used.
#[derive(Debug, PartialEq, Eq)]
struct Holdings {
    runs: Vec<Vec<Range<u64>>>,
    free: u64,
    used: Vec<u64>,
}

fn holdings(fs: &mut ARXFS<MemBlock>) -> Holdings {
    let alloc = fs.allocator().expect("writable");
    let runs = [
        &alloc.pending_used,
        &alloc.pending_free,
        &alloc.txn_private,
        &alloc.op_claimed,
        &alloc.op_released,
        &alloc.op_deferred,
        &alloc.txn_freed,
        &alloc.embargo,
        &alloc.pending_discard,
        &alloc.reserve,
        &alloc.txn_reserve,
        &alloc.op_reserve_taken,
        &alloc.op_reserve_returned,
    ]
    .into_iter()
    .map(|set| set.iter().collect())
    .collect();
    let free = fs.free_count;
    let used = (0..fs.total_blocks)
        .filter(|&block| fs.bit_used(block).expect("read the map"))
        .collect();
    Holdings { runs, free, used }
}

/// [`holdings`] once every deferred mark is folded in, so two volumes whose
/// marks were deferred differently compare by what they mark.
fn folded(fs: &mut ARXFS<MemBlock>) -> Holdings {
    fs.map_fold_pending().expect("fold the deferred marks");
    holdings(fs)
}

/// Hold the map, once folded, to accounting for each block once: free or used.
fn conserved(fs: &mut ARXFS<MemBlock>) {
    let held = folded(fs);
    let used = u64::try_from(held.used.len()).expect("a count of blocks");
    assert_eq!(
        held.free + used,
        fs.total_blocks,
        "a block is free or used, once"
    );
}

/// Run `step` with no allocation granted, then one more each time until it
/// goes through, holding each refusal to an `OutOfMemory` that changed
/// nothing: answers the grants it took.
fn refused_until_done(
    fs: &mut ARXFS<MemBlock>,
    mut step: impl FnMut(&mut ARXFS<MemBlock>) -> Result<(), DriverError>,
) -> usize {
    for granted in 0..64 {
        let before = holdings(fs);
        match refusing_after(granted, || step(fs)) {
            Ok(()) => {
                conserved(fs);
                return granted;
            }
            Err(err) => {
                assert_eq!(
                    err,
                    DriverError::OutOfMemory,
                    "refused after {granted} grants"
                );
                assert_eq!(holdings(fs), before, "refused after {granted} grants");
                assert!(!fs.read_only, "a refusal is an answer, not a freeze");
            }
        }
    }
    panic!("the step never went through");
}

/// Blocks the tests mark: `free` and its two successors are free and outside
/// the reserve, and `data` and its two successors hold a published file.
struct Marks {
    free: u64,
    data: u64,
}

/// A batched volume holding a published file, the map's pages dropped so
/// every mark is deferred, and an operation open on it. The transaction's
/// sets and the operation's records start with no room, so the first step of
/// each kind must take some from the heap.
fn cold_opened() -> (ARXFS<MemBlock>, Marks) {
    let (mut fs, _host) = fmt_batched_with_host(512, 1 << 12, 64);
    let root = fs.root();
    let file = fs
        .create(root, b"kept", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.write_at(file, 0, &incompressible(4096)).expect("write");
    fs.flush().expect("publish");
    let (_, extent) = extent_of(&mut fs, b"kept", 0);
    assert!(
        extent.len >= 3,
        "the fixture's data is one run of {}",
        extent.len
    );
    let free = (RING_BLOCKS..fs.total_blocks)
        .find(|&block| {
            (block..block + 3).all(|at| {
                let reserved = fs.allocator().expect("writable").reserve.contains(at);
                !reserved && !fs.bit_used(at).expect("read the map")
            })
        })
        .expect("three free blocks outside the reserve");
    // A sync leaves only clean pages resident, so dropping them loses nothing.
    fs.allocator_mut().expect("writable").cache.clear();
    fs.begin().expect("begin");
    let at = Marks {
        free,
        data: extent.phys,
    };
    (fs, at)
}

#[test]
fn a_claim_refused_for_memory_changes_nothing() {
    let (mut fs, at) = cold_opened();
    assert!(refused_until_done(&mut fs, |fs| fs.claim_run(at.free, 1)) > 0);
    assert!(fs.bit_used(at.free).expect("read the map"));
}

#[test]
fn a_draw_on_the_reserve_refused_for_memory_changes_nothing() {
    let (mut fs, _) = cold_opened();
    fs.draw = Draw::System;
    assert!(refused_until_done(&mut fs, |fs| fs.take_reserved(2).map(drop)) > 0);
}

#[test]
fn a_deferred_free_refused_for_memory_changes_nothing() {
    let (mut fs, at) = cold_opened();
    assert!(refused_until_done(&mut fs, |fs| fs.free_run(at.data, 1)) > 0);
    assert_eq!(fs.allocator().expect("writable").txn_freed.covered(), 1);
}

#[test]
fn a_release_refused_for_memory_changes_nothing() {
    // The pair an earlier operation claimed, freed by a later one: out of the
    // private set, free again, and recorded for the later one's undo.
    let (mut fs, _) = cold_opened();
    let pair = fs
        .alloc_block(true)
        .expect("an earlier operation claims a pair");
    fs.end_operation().expect("and joins the transaction");
    fs.begin().expect("a later operation");
    assert!(refused_until_done(&mut fs, |fs| fs.free_meta(pair)) > 0);
    assert!(!fs.bit_used(pair).expect("read the map"));
}

/// An earlier operation of `fs`'s transaction: claims either side of the block
/// a later one claims, a pair drawn from the reserve, and deferrals either side
/// of the block a later one defers, joined before the later operation opens.
/// Answers the pair.
fn earlier_operation(fs: &mut ARXFS<MemBlock>, at: &Marks) -> u64 {
    fs.draw = Draw::System;
    fs.claim_run(at.free, 1).expect("claim");
    fs.claim_run(at.free + 2, 1).expect("claim");
    let drawn = fs.take_reserved(2).expect("draw on the reserve");
    fs.defer_free(at.data, 1).expect("defer");
    fs.defer_free(at.data + 2, 1).expect("defer");
    fs.end_operation()
        .expect("the operation joins the transaction");
    fs.begin().expect("a later operation");
    drawn
}

#[test]
fn undoing_an_operation_asks_the_heap_for_nothing() {
    // Undoing the later operation splits a private run, a deferred run and a
    // draw back apart, and every mark it undoes was deferred.
    let (mut untouched, at) = cold_opened();
    earlier_operation(&mut untouched, &at);
    let (mut fs, at) = cold_opened();
    let drawn = earlier_operation(&mut fs, &at);
    fs.claim_run(at.free + 1, 1).expect("claim between");
    fs.free_meta(drawn).expect("release the earlier draw");
    fs.defer_free(at.data + 1, 1).expect("defer between");
    let ((), metering) = metered(|| refusing_above(0, || fs.rollback()));
    assert_eq!(metering.allocations, 0, "{metering:?}");
    assert!(!fs.read_only, "the undo met no refusal");
    assert_eq!(folded(&mut fs), folded(&mut untouched), "undone exactly");
    conserved(&mut fs);
}

#[test]
fn abandoning_a_transaction_asks_the_heap_for_nothing() {
    let (mut untouched, _) = cold_opened();
    untouched.abort_transaction();
    let (mut fs, at) = cold_opened();
    fs.draw = Draw::System;
    fs.claim_run(at.free, 1).expect("claim");
    fs.claim_run(at.free + 2, 1).expect("claim");
    fs.take_reserved(2).expect("draw on the reserve");
    fs.defer_free(at.data, 1).expect("defer");
    fs.defer_free(at.data + 2, 1).expect("defer");
    let ((), metering) = metered(|| refusing_above(0, || fs.abort_transaction()));
    assert_eq!(metering.allocations, 0, "{metering:?}");
    assert!(!fs.read_only, "nothing reported into it was lost");
    assert_eq!(
        folded(&mut fs),
        folded(&mut untouched),
        "nothing of it is left"
    );
    conserved(&mut fs);
}

#[test]
fn a_buffer_refused_memory_is_out_of_memory_not_a_full_volume() {
    // Only a full volume earns the release-and-retry a mutation gives one.
    let walk = refusing_above(0, || TreeWalk::new(512).err());
    assert_eq!(walk, Some(DriverError::OutOfMemory));
    let scan = refusing_above(0, || DirScan::new(512).err());
    assert_eq!(scan, Some(DriverError::OutOfMemory));
}

#[test]
fn a_cluster_frame_refused_memory_is_an_answer() {
    let mut fs = fmt(512, 1 << 12, 64);
    let cluster = alloc::vec![0x42u8; as_usize(fs.data_capacity() * COMPRESS_CLUSTER_BLOCKS)];
    let refused = refusing_above(1024, || fs.write_data_cluster(&cluster));
    assert_eq!(refused, Err(DriverError::OutOfMemory));
}

/// The largest allocation a short heap grants: room for a few dozen runs a
/// set, far fewer than [`INTERLEAVED`] separate ones take.
const SHORT_HEAP: usize = 8 * 1024;

/// Blocks each of [`interleaved`]'s files holds.
const INTERLEAVED: usize = 160;

/// A volume whose files `a` and `b` took turns at the device a block at a
/// time, so every block of either is a run of its own, all published; with the
/// bytes each holds.
fn interleaved() -> (ARXFS<MemBlock>, [(NodeId, Vec<u8>); 2]) {
    let mut fs = fmt(512, 1 << 13, 64);
    let root = fs.root();
    let cap = as_usize(fs.data_capacity());
    let mut files = [&b"a"[..], b"b"].map(|name| {
        let node = fs
            .create(root, name, NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        (node, Vec::new())
    });
    let mut source = tairix_fuzzseed::Prng::new(0x1EAF_0001);
    for block in 0..INTERLEAVED {
        for (node, held) in &mut files {
            let mut bytes = alloc::vec![0u8; cap];
            source.fill(&mut bytes);
            let at = u64::try_from(block * cap).expect("in range");
            assert_eq!(fs.write_at(*node, at, &bytes), Ok(cap));
            held.extend_from_slice(&bytes);
        }
    }
    fs.flush().expect("publish");
    (fs, files)
}

/// Hold a volume an operation was refused on to what it was: once published,
/// the same free space and each file's bytes, and a check that rebuilds the
/// map from the trees finds the one it held.
fn unchanged(fs: &mut ARXFS<MemBlock>, before: &VolumeStats, files: &[(NodeId, Vec<u8>)]) {
    assert!(!fs.read_only, "a refusal is an answer, not a freeze");
    fs.flush().expect("publish");
    assert_eq!(fs.stats().expect("stats"), *before);
    for (node, bytes) in files {
        assert_eq!(&read_all(fs, *node, bytes.len()), bytes);
    }
    let report = check_full(fs);
    assert_eq!(report.structure, StructureVerdict::Sound, "{report:?}");
    assert_eq!(report.unrecoverable_findings, 0, "{report:?}");
    assert_eq!(
        fs.stats().expect("stats"),
        *before,
        "the rebuilt map is the one held"
    );
}

#[test]
fn a_write_refused_for_memory_leaves_the_volume_as_it_was() {
    let (mut fs, files) = interleaved();
    let before = fs.stats().expect("stats");
    // Rewriting the whole of the first file releases each of its blocks as a
    // run of its own, more than the short heap can record.
    let first = files[0].0;
    let mut rewrite = alloc::vec![0u8; files[0].1.len()];
    tairix_fuzzseed::Prng::new(0x1EAF_0002).fill(&mut rewrite);
    let refused = refusing_above(SHORT_HEAP, || fs.write_at(first, 0, &rewrite));
    assert_eq!(refused, Err(DriverError::OutOfMemory));
    unchanged(&mut fs, &before, &files);
    assert_eq!(fs.write_at(first, 0, &rewrite), Ok(rewrite.len()));
    fs.flush().expect("publish the rewrite");
    assert_eq!(read_all(&mut fs, first, rewrite.len()), rewrite);
    assert_eq!(check_full(&mut fs).structure, StructureVerdict::Sound);
}

#[test]
fn a_remove_refused_for_memory_leaves_the_name_and_its_blocks() {
    let (mut fs, files) = interleaved();
    let before = fs.stats().expect("stats");
    let root = fs.root();
    let refused = refusing_above(SHORT_HEAP, || fs.remove(root, b"b"));
    assert_eq!(refused, Err(DriverError::OutOfMemory));
    unchanged(&mut fs, &before, &files);
    fs.remove(root, b"b").expect("the retry goes through");
    fs.flush().expect("publish the remove");
    assert_eq!(fs.lookup(root, b"b"), Err(DriverError::NotFound));
    assert!(fs.stats().expect("stats").free_blocks > before.free_blocks);
    assert_eq!(check_full(&mut fs).structure, StructureVerdict::Sound);
}

#[test]
fn a_fold_cut_short_keeps_every_mark_it_has_yet_to_apply() {
    let (mut fs, _) = cold_opened();
    // A claim across a page boundary while neither page is resident, which a
    // fold applies a page at a time.
    let boundary = fs.allocator().expect("writable").geom.page_first_block(1);
    let run = boundary - 2..boundary + 2;
    assert!(
        run.clone()
            .all(|block| !fs.bit_used(block).expect("read the map")),
        "the run is free"
    );
    fs.allocator_mut().expect("writable").cache.clear();
    fs.claim_run(run.start, run.end - run.start).expect("claim");
    for granted in 0..64 {
        let folded = refusing_after(granted, || fs.map_fold_pending());
        assert!(
            run.clone()
                .all(|block| fs.bit_used(block).expect("read the map")),
            "a fold refused after {granted} grants lost part of the claim"
        );
        match folded {
            Ok(()) => {
                conserved(&mut fs);
                return;
            }
            Err(err) => {
                assert_eq!(
                    err,
                    DriverError::OutOfMemory,
                    "refused after {granted} grants"
                );
            }
        }
    }
    panic!("the fold never went through");
}

/// A file a batch wrote, and the bytes it holds.
type Written = (NodeId, Vec<u8>);

/// A batching volume whose open transaction created and wrote two files —
/// four operations reported, none published — with each file's bytes.
fn reported_batch() -> (ARXFS<MemBlock>, &'static TestVolumeHost, [Written; 2]) {
    let (mut fs, host) = fmt_batched_with_host(512, 1 << 12, 64);
    let root = fs.root();
    let files = [(&b"one"[..], 0x0B47_0001), (&b"two"[..], 0x0B47_0002)].map(|(name, seed)| {
        let node = fs
            .create(root, name, NodeKind::RegularFile, &FILE_RECORD)
            .expect("create");
        let mut bytes = alloc::vec![0u8; 1500];
        tairix_fuzzseed::Prng::new(seed).fill(&mut bytes);
        assert_eq!(fs.write_at(node, 0, &bytes), Ok(bytes.len()));
        (node, bytes)
    });
    assert_eq!(fs.schedule.acknowledged(), 4, "the batch is open");
    // The last operation's undo records are the transaction's now; dropped as
    // the next operation, or a commit of its own, drops them.
    fs.clear_op_state();
    (fs, host, files)
}

/// The blocks the open transaction staged, with their bytes.
fn staged(fs: &ARXFS<MemBlock>) -> Vec<(u64, Vec<u8>)> {
    fs.dirty
        .blocks(WritePhase::BeforeBarrier)
        .map(|(phys, bytes)| (phys, bytes.to_vec()))
        .collect()
}

/// Every write of `files` reads back from a remount of `fs`'s device, and the
/// remounted volume is sound.
fn published(fs: &mut ARXFS<MemBlock>, files: &[Written]) {
    let mut disk = reopen_device(fs, 512, 1 << 12);
    for (node, bytes) in files {
        assert_eq!(&read_all(&mut disk, *node, bytes.len()), bytes);
    }
    let report = check_full(&mut disk);
    assert_eq!(report.structure, StructureVerdict::Sound, "{report:?}");
}

#[test]
fn a_commit_refused_for_memory_keeps_its_batch_for_the_next_to_publish() {
    let (mut fs, _host, files) = reported_batch();
    let before = (staged(&fs), folded(&mut fs));
    let generation = fs.generation;
    for granted in 0..256 {
        let Err(err) = refusing_after(granted, || fs.commit()) else {
            assert!(granted > 0, "a commit asks the heap for room");
            assert_eq!(fs.generation, generation + 1);
            published(&mut fs, &files);
            return;
        };
        assert_eq!(
            err,
            DriverError::OutOfMemory,
            "refused after {granted} grants"
        );
        assert!(!fs.read_only, "a refusal is an answer, not a freeze");
        assert!(fs.schedule.is_open(), "the batch stays open");
        assert_eq!(fs.schedule.acknowledged(), 4);
        assert_eq!(fs.generation, generation, "nothing was published");
        assert_eq!(
            (staged(&fs), folded(&mut fs)),
            before,
            "refused after {granted} grants"
        );
    }
    panic!("the commit never went through");
}

#[test]
fn an_operation_whose_close_is_refused_goes_and_the_batch_it_joined_stays() {
    let (mut fs, host, files) = reported_batch();
    let window = writeback_window_ns(fs.block_mut().command_cost(), PressureBand::Normal);
    let before = (staged(&fs), folded(&mut fs));
    let generation = fs.generation;
    let root = fs.root();
    // Each attempt runs past its transaction's window, so its close publishes.
    let mut now = PAST_EVERY_WINDOW;
    for granted in 0..512 {
        host.set_now(now);
        let late = refusing_after(granted, || {
            fs.create(root, b"late", NodeKind::RegularFile, &FILE_RECORD)
        });
        let Err(err) = late else {
            assert_eq!(
                fs.generation,
                generation + 1,
                "the close published the batch"
            );
            published(&mut fs, &files);
            let mut disk = reopen_device(&mut fs, 512, 1 << 12);
            let disk_root = disk.root();
            disk.lookup(disk_root, b"late")
                .expect("the late name was published");
            return;
        };
        assert_eq!(
            err,
            DriverError::OutOfMemory,
            "refused after {granted} grants"
        );
        assert!(!fs.read_only, "a refusal is an answer, not a freeze");
        assert_eq!(fs.lookup(root, b"late"), Err(DriverError::NotFound));
        assert_eq!(fs.schedule.acknowledged(), 4, "the batch it joined stays");
        assert_eq!(fs.generation, generation, "nothing was published");
        assert_eq!(
            (staged(&fs), folded(&mut fs)),
            before,
            "refused after {granted} grants"
        );
        now += window;
    }
    panic!("the operation never went through");
}

#[test]
fn a_sync_refused_for_memory_is_come_back_for_a_window_later() {
    let (mut fs, host, files) = reported_batch();
    let window = writeback_window_ns(fs.block_mut().command_cost(), PressureBand::Normal);
    host.set_now(PAST_EVERY_WINDOW);
    assert_eq!(
        refusing_after(0, || fs.flush()),
        Err(DriverError::OutOfMemory)
    );
    assert_eq!(
        host.reported(),
        Some(PAST_EVERY_WINDOW + window),
        "the timer is armed a window on, never in the past"
    );
    host.set_now(PAST_EVERY_WINDOW + window);
    fs.flush()
        .expect("the timer's next sync publishes the batch");
    assert_eq!(host.reported(), None, "nothing is left to publish");
    published(&mut fs, &files);
}

#[test]
fn a_device_fault_after_a_refused_commit_still_gives_the_batch_up() {
    let (mut fs, _host, _) = reported_batch();
    assert_eq!(
        refusing_after(0, || fs.commit()),
        Err(DriverError::OutOfMemory)
    );
    assert!(!fs.read_only);
    fs.block_mut().write_fault_after = Some(0);
    assert!(
        fs.flush().is_err(),
        "a sync the device refuses fails closed"
    );
    assert!(
        fs.read_only,
        "a handle that lost reported work stops writing"
    );
    fs.block_mut().write_fault_after = None;
    let mut disk = reopen_device(&mut fs, 512, 1 << 12);
    let disk_root = disk.root();
    assert_eq!(disk.lookup(disk_root, b"one"), Err(DriverError::NotFound));
    assert_eq!(check_full(&mut disk).structure, StructureVerdict::Sound);
}

/// A batched 100 TiB volume holding a reported name, with an operation open
/// that has changed bitmap pages until the cache is full and the page it would
/// evict next holds changes; the device already holds a block for each one
/// that eviction writes, so writing them asks the heap for nothing. Answers
/// the volume, a block the evicted page marked and the first block of the page
/// next to read.
fn crowded_map() -> (ARXFS<SparseBlock>, u64, u64) {
    let mut fs = fmt_huge().with_volume_host(TestVolumeHost::volume(), TestVolumeHost::leaked(0));
    let root = fs.root();
    fs.create(root, b"reported", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    fs.begin().expect("begin");
    let geom = fs.allocator().expect("writable").geom;
    let pages = u64::try_from(2 * MAX_CACHED_PAGES).expect("a page count");
    for page in 1..=pages {
        fs.mark_range_used(geom.page_first_block(page), 1)
            .expect("mark");
        let next = fs.allocator().expect("writable").cache.eviction_candidate();
        let Some((victim, true)) = next else {
            continue;
        };
        // A summary block may come up first; only a bitmap page this loop
        // marked names a block to look for afterwards.
        let Some(evicted) = victim
            .checked_sub(geom.page_block(0))
            .filter(|&evicted| (1..=page).contains(&evicted))
        else {
            continue;
        };
        let bs = fs.block_size;
        for written in [geom.header_block(), victim] {
            fs.block_mut()
                .blocks
                .entry(written)
                .or_insert_with(|| alloc::vec![0; bs]);
        }
        return (
            fs,
            geom.page_first_block(evicted),
            geom.page_first_block(page + 1),
        );
    }
    panic!("no page holding changes ever came to be evicted next");
}

#[test]
fn an_eviction_refused_its_staging_keeps_the_page_and_needs_no_rebuild() {
    let (mut fs, first, next) = crowded_map();
    for granted in 0..64 {
        let Err(err) = refusing_after(granted, || fs.mark_range_used(next, 1)) else {
            assert!(fs.bit_used(next).expect("read the map"));
            assert!(
                fs.bit_used(first).expect("read the map"),
                "the evicted page took its change with it"
            );
            return;
        };
        assert_eq!(
            err,
            DriverError::OutOfMemory,
            "refused after {granted} grants"
        );
        assert!(!fs.read_only, "the batch the name joined stands");
        assert!(
            !fs.allocator().expect("writable").needs_rebuild,
            "a refused staging wrote nothing, so the map is still trusted"
        );
    }
    panic!("the mark never went through");
}

#[test]
fn a_cluster_read_refused_memory_answers_it_and_changes_nothing() {
    let mut fs = fmt(512, 1 << 12, 64);
    let root = fs.root();
    let file = fs
        .create(root, b"packed", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let cluster = compressible_cluster(&fs);
    assert_eq!(fs.write_at(file, 0, &cluster), Ok(cluster.len()));
    let (_, ext) = extent_of(&mut fs, b"packed", 0);
    assert!(ext.compressed, "the file is one compressed cluster");
    let before = holdings(&mut fs);
    let mut back = alloc::vec![0u8; cluster.len()];
    let read = (0..64).find_map(|granted| {
        match refusing_after(granted, || fs.read_at(file, 0, &mut back)) {
            Ok(read) => Some(read),
            Err(err) => {
                assert_eq!(
                    err,
                    DriverError::OutOfMemory,
                    "refused after {granted} grants"
                );
                assert_eq!(holdings(&mut fs), before, "a read changes nothing");
                None
            }
        }
    });
    assert_eq!(read, Some(cluster.len()), "the read went through");
    assert_eq!(back, cluster);
    // Only the plaintext is refused: the frame and every other buffer fit
    // beneath it.
    let short = cluster.len() - 1;
    assert_eq!(
        refusing_above(short, || fs.read_data_cluster_classified(&ext).err()),
        Some(DataFault::OutOfMemory)
    );
    assert_eq!(
        refusing_above(short, || {
            fs.scrub(&GrantAll, &NullSink, ScrubBudget::Unlimited).err()
        }),
        Some(DriverError::OutOfMemory),
        "a pass refused a buffer answers the refusal, not a fault"
    );
    let report = fs
        .scrub(&GrantAll, &NullSink, ScrubBudget::Unlimited)
        .expect("scrub");
    let faults = (
        report.data_physical_faults,
        report.data_seal_faults,
        report.data_logical_faults,
    );
    assert_eq!(faults, (0, 0, 0), "{report:?}");
}

#[test]
fn a_rescue_refused_a_buffer_stops_rather_than_skip_readable_data() {
    let mut fs = fmt(512, 1 << 12, 64);
    let root = fs.root();
    let file = fs
        .create(root, b"packed", NodeKind::RegularFile, &FILE_RECORD)
        .expect("create");
    let cluster = compressible_cluster(&fs);
    assert_eq!(fs.write_at(file, 0, &cluster), Ok(cluster.len()));
    let bytes = fs.into_block().expect("the volume closes").bytes();
    let device = MemBlock::from_bytes(bytes, 512, 1 << 12);
    let mut out = CollectSink::new();
    let rescued = refusing_above(cluster.len() - 1, || {
        ARXFS::rescue(device, TEST_KEYING, &GrantAll, &NullSink, &mut out)
    });
    assert_eq!(rescued.err(), Some(DriverError::OutOfMemory));
}

/// The room a step keeps for the paths that cannot fail is pinned with the
/// runs, so a transaction holding little but room still counts it against the
/// ceiling.
#[test]
fn the_room_kept_for_an_undo_is_pinned_with_the_runs() {
    let (mut fs, _) = cold_opened();
    let alloc = fs.allocator_mut().expect("writable");
    let (bytes, runs) = alloc.txn_pinned();
    alloc.keep_room(1).expect("room");
    let (roomy, counted) = alloc.txn_pinned();
    assert_eq!(counted, runs, "no run was recorded");
    assert!(
        roomy > bytes,
        "{roomy} bytes pinned against {bytes} before the room was made"
    );
}

/// Once its transaction is published, the sets it drained keep the room it
/// kept for its undo while memory is plentiful, for the next transaction, and
/// give it back once memory is short.
#[test]
fn the_room_a_published_transaction_kept_goes_back_once_memory_is_short() {
    static GAUGES: [tairix_reclaim::ReportedPressure; 2] = [
        tairix_reclaim::ReportedPressure::unknown(),
        tairix_reclaim::ReportedPressure::unknown(),
    ];
    let bands = [
        (PressureBand::Normal, true),
        (PressureBand::Moderate, false),
    ];
    for (gauge, (band, kept)) in GAUGES.iter().zip(bands) {
        let (fs, _) = interleaved();
        gauge.report(band);
        let mut fs = fs
            .with_writeback_bound(1 << 30, gauge, Arc::new(PinnedAccounting::new()))
            .expect("bounded");
        let root = fs.root();
        // Freeing a file of separate runs keeps room for a mark of each, which
        // the commit's fold leaves unused.
        fs.remove(root, b"b").expect("the remove publishes");
        let alloc = fs.allocator().expect("writable");
        let drained = [&alloc.pending_used, &alloc.pending_free];
        assert!(drained.iter().all(|set| set.is_empty()), "{band:?}");
        let held: usize = drained.iter().map(|set| set.allocated_bytes()).sum();
        assert_eq!(held > 0, kept, "{band:?}: {held} bytes kept");
    }
}

/// The room the outliving sets hold beyond their runs stays while it is within
/// what may be retained, and goes back whole once it passes that.
#[test]
fn spare_room_past_what_may_be_retained_goes_back() {
    let (mut fs, _) = cold_opened();
    let alloc = fs.allocator_mut().expect("writable");
    alloc.pending_used.try_reserve(64).expect("room");
    alloc.pending_free.try_reserve(64).expect("room");
    let held = |alloc: &Allocator| {
        [
            &alloc.pending_used,
            &alloc.pending_free,
            &alloc.reserve,
            &alloc.embargo,
        ]
        .iter()
        .map(|set| set.allocated_bytes())
        .sum::<usize>()
    };
    let before = held(alloc);
    alloc.give_back_room(before);
    assert_eq!(held(alloc), before, "within what may be retained");
    alloc.give_back_room(0);
    assert_eq!(
        (
            alloc.pending_used.allocated_bytes(),
            alloc.pending_free.allocated_bytes()
        ),
        (0, 0),
        "past it, the drained sets keep none"
    );
}
