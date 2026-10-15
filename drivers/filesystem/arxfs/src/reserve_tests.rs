//! The reserve: what ordinary allocation leaves alone, who may draw on it, how
//! what is drawn comes back, and how large it is held.

use super::*;
use crate::reserve::{Draw, OPERATION_SLACK};

const MIB: u64 = 1 << 20;

/// 512-byte blocks spanning `bytes`.
const fn blocks(bytes: u64) -> u64 {
    bytes / 512
}

/// Fill the volume with ordinary writes, a file at a time and then a block at
/// a time, until it refuses.
fn fill(fs: &mut ARXFS<MemBlock>) {
    let root = fs.root();
    let body = alloc::vec![0x5Au8; 4096];
    let step = fs.data_capacity();
    for file in 0.. {
        assert!(file < 100_000, "the volume never filled");
        let name = alloc::format!("fill{file}");
        if fs
            .create(root, name.as_bytes(), NodeKind::RegularFile)
            .is_err()
        {
            return;
        }
        if fs.write_at(root, name.as_bytes(), 0, &body).is_err() {
            let mut at = 0;
            while fs.write_at(root, name.as_bytes(), at, &[1]).is_ok() {
                at += step;
            }
            return;
        }
    }
}

fn reserve_held(fs: &ARXFS<MemBlock>) -> u64 {
    fs.allocator().expect("writable").reserve.covered()
}

#[test]
fn an_ordinary_writer_stops_at_the_reserve_and_leaves_it_whole() {
    let mut fs = fmt(512, 1 << 12, 64);
    let reserve = fs.reserve_blocks();
    assert_eq!(reserve, release_share_blocks(512, 1 << 12));
    assert_eq!(reserve_held(&fs), reserve, "a fresh volume holds it whole");

    fill(&mut fs);
    fs.flush().expect("publish");
    let stats = fs.stats().expect("stats");
    // What a full volume still reports outside its reserve is what ordinary
    // allocation could not pair for metadata, less than an operation's worth.
    assert!(stats.avail_blocks < OPERATION_SLACK, "{stats:?}");
    assert!(fs.in_reserve());
    assert!(stats.free_blocks >= reserve, "the reserve is still free");
    assert_eq!(reserve_held(&fs), reserve, "and still held whole");
    let root = fs.root();
    assert_eq!(
        fs.create(root, b"more", NodeKind::RegularFile),
        Err(DriverError::NoSpace)
    );
}

#[test]
fn the_system_writes_into_the_reserve_once_users_have_filled_the_volume() {
    let mut fs = fmt(512, 1 << 12, 64);
    fill(&mut fs);
    let root = fs.root();

    fs.set_allocation_class(AllocationClass::System);
    fs.create(root, b"settings", NodeKind::RegularFile)
        .expect("a system file is created on a full volume");
    fs.write_at(root, b"settings", 0, &[7u8; 8192])
        .expect("and written");
    fs.flush().expect("and its work commits");
    fs.set_allocation_class(AllocationClass::Ordinary);

    assert_eq!(
        fs.create(root, b"user", NodeKind::RegularFile),
        Err(DriverError::NoSpace),
        "a user is still refused"
    );
    let node = fs
        .lookup(root, b"settings")
        .expect("the system file stands");
    assert_eq!(read_all(&mut fs, node, 8192), alloc::vec![7u8; 8192]);
}

#[test]
fn a_delete_on_a_full_volume_frees_space_for_the_next_write() {
    let mut fs = fmt(512, 1 << 12, 64);
    fill(&mut fs);
    fs.flush().expect("publish");
    let root = fs.root();

    fs.remove(root, b"fill0")
        .expect("a full volume still deletes");
    fs.flush().expect("publish the delete");
    assert!(fs.stats().expect("stats").avail_blocks > 0);
    assert_eq!(
        reserve_held(&fs),
        fs.reserve_blocks(),
        "the delete drew nothing it kept"
    );
    fs.create(root, b"after", NodeKind::RegularFile)
        .expect("an ordinary create succeeds again");
    fs.write_at(root, b"after", 0, &[3u8; 1024])
        .expect("and an ordinary write");
}

#[test]
fn an_ordinary_operation_cannot_draw_on_the_reserve() {
    let mut fs = fmt(512, 1 << 12, 64);
    fs.begin().expect("begin");
    assert_eq!(fs.draw, Draw::Ordinary);
    assert_eq!(fs.take_reserved(2), Err(DriverError::NoSpace));
    assert_eq!(fs.take_reserved(1), Err(DriverError::NoSpace));
    fs.rollback();
    assert_eq!(reserve_held(&fs), fs.reserve_blocks());
}

#[test]
fn a_release_that_holds_more_than_it_frees_is_refused() {
    let mut fs = fmt(512, 1 << 12, 64);
    let before = reserve_held(&fs);
    fs.draw = Draw::Release;
    fs.begin().expect("begin");
    let pair = fs.take_reserved(2).expect("a release may take a pair");
    assert_eq!(
        fs.admit_release(),
        Err(DriverError::NoSpace),
        "it frees nothing, so it may not keep what it took"
    );
    fs.rollback();
    fs.draw = Draw::Ordinary;
    assert_eq!(
        reserve_held(&fs),
        before,
        "the undone draw is back in the reserve"
    );
    assert!(fs.allocator().expect("writable").reserve.contains(pair));
    assert!(!fs.bit_used(pair).expect("map"), "and free again");
}

#[test]
fn a_release_that_frees_what_it_takes_is_admitted() {
    let mut fs = fmt(512, 1 << 12, 64);
    let root = fs.root();
    fs.create(root, b"victim", NodeKind::RegularFile)
        .expect("create");
    fs.write_at(root, b"victim", 0, &[9u8; 4096])
        .expect("write");
    fs.flush().expect("publish");
    let ino = file_ino(&mut fs, b"victim");
    let extent_root = fs.read_inode(ino).expect("inode").extent_root;

    fs.draw = Draw::Release;
    fs.begin().expect("begin");
    fs.take_reserved(2).expect("take a pair");
    // Release the file's extent tree root: two committed blocks deferred for
    // freeing, as much as the pair taken.
    fs.free_meta(extent_root);
    assert_eq!(fs.admit_release(), Ok(()));
    fs.rollback();
    fs.draw = Draw::Ordinary;
}

#[test]
fn an_abandoned_transaction_gives_its_draws_back() {
    let mut fs = fmt(512, 1 << 12, 64);
    let before = reserve_held(&fs);
    fs.draw = Draw::System;
    fs.begin().expect("begin");
    fs.take_reserved(2).expect("the system takes a pair");
    fs.take_reserved(1).expect("and a data block");
    assert_eq!(reserve_held(&fs), before - 3);
    fs.abort_transaction();
    fs.draw = Draw::Ordinary;
    assert_eq!(reserve_held(&fs), before);
    assert!(fs.allocator().expect("writable").txn_reserve.is_empty());
}

#[test]
fn a_draw_freed_in_its_own_transaction_returns_to_the_reserve() {
    let mut fs = fmt(512, 1 << 12, 64);
    let before = reserve_held(&fs);
    fs.draw = Draw::System;
    fs.begin().expect("begin");
    let pair = fs.take_reserved(2).expect("take a pair");
    fs.free_meta(pair);
    assert_eq!(
        reserve_held(&fs),
        before,
        "a private block freed goes straight back"
    );
    let alloc = fs.allocator().expect("writable");
    assert!(alloc.op_reserve_taken.is_empty() && alloc.op_reserve_returned.is_empty());
    fs.rollback();
    fs.draw = Draw::Ordinary;
    assert_eq!(
        reserve_held(&fs),
        before,
        "and undoing the operation changes nothing"
    );
}

#[test]
fn undoing_a_return_takes_the_block_back_for_the_operation_that_drew_it() {
    let (mut fs, _host) = fmt_batched_with_host(512, 1 << 12, 64);
    let before = reserve_held(&fs);
    fs.draw = Draw::System;
    fs.begin().expect("begin");
    let pair = fs
        .take_reserved(2)
        .expect("an earlier operation takes a pair");
    fs.end_operation().expect("and joins the transaction");
    assert!(fs.schedule.is_open(), "the batch is still open");

    fs.begin().expect("a later operation");
    fs.free_meta(pair);
    assert_eq!(reserve_held(&fs), before, "freeing it returns it");
    fs.rollback();
    fs.draw = Draw::Ordinary;

    let alloc = fs.allocator().expect("writable");
    assert!(
        !alloc.reserve.contains(pair),
        "undone, it is the earlier draw again"
    );
    assert!(alloc.txn_reserve.contains(pair));
    assert!(alloc.txn_private.contains(pair));
    assert!(fs.bit_used(pair).expect("map"));
}

#[test]
fn a_full_volume_holding_a_pending_delete_still_mounts() {
    let mut fs = spanning_delete_volume();
    fill(&mut fs);
    let root = fs.root();
    let ino = u64::from(file_ino(&mut fs, b"frag"));
    fs.draw = Draw::Release;
    fs.begin().expect("begin");
    fs.remove_inner(root, b"frag").expect("detach the name");
    fs.draw = Draw::Ordinary;
    fs.flush().expect("publish the interrupted delete");
    assert_eq!(pending_deletes(&mut fs), alloc::vec![ino]);
    let bytes = fs.into_block().expect("the volume closes").bytes();

    let mut fs = ARXFS::open(MemBlock::from_bytes(bytes, 512, 1 << 14), TEST_KEYING)
        .expect("the full volume still mounts");
    assert!(
        pending_deletes(&mut fs).is_empty(),
        "and finished the delete"
    );
    assert!(
        fs.stats().expect("stats").avail_blocks > 0,
        "whose space is ordinary space again"
    );
}

#[test]
fn a_reclaim_that_fails_after_a_committed_unlink_leaves_the_unlink_standing() {
    let mut fs = spanning_delete_volume();
    let root = fs.root();
    let ino = u64::from(file_ino(&mut fs, b"frag"));
    fs.release(|fs| fs.remove_inner(root, b"frag"))
        .expect("the unlink commits");
    fs.block_mut().fail_flush = true;
    assert_eq!(
        fs.finish_reclaim(),
        Ok(()),
        "the name has gone, so the reclaim's failure is not the unlink's"
    );
    assert_eq!(fs.lookup(root, b"frag"), Err(DriverError::NotFound));
    assert_eq!(pending_deletes(&mut fs), alloc::vec![ino]);
}

/// `fs` with each of `large` detached and published pending, then a small file
/// whose unlink has joined a batch whose window has lapsed: the drain that
/// follows the unlink publishes the batch in its first step.
fn batched_unlink_behind(mut fs: ARXFS<MemBlock>, large: &[&[u8]]) -> ARXFS<MemBlock> {
    let root = fs.root();
    for name in large {
        fs.begin().expect("begin");
        fs.remove_inner(root, name)
            .expect("detach a large node's name");
        fs.flush().expect("publish it pending");
    }
    fs.create(root, b"small", NodeKind::RegularFile)
        .expect("create");
    fs.flush().expect("publish");
    let host = TestVolumeHost::leaked(0);
    let mut fs = fs.with_volume_host(TestVolumeHost::volume(), host);
    fs.release(|fs| fs.remove_inner(root, b"small"))
        .expect("the unlink joins the batch");
    assert!(fs.schedule.is_open());
    host.set_now(1 << 62);
    fs
}

#[test]
fn a_reclaim_that_abandons_a_batched_unlink_reports_it() {
    let mut fs = batched_unlink_behind(spanning_delete_volume(), &[b"frag"]);
    fs.block_mut().fail_flush = true;
    assert!(
        fs.finish_reclaim().is_err(),
        "the transaction carrying the unlink was abandoned"
    );
    fs.block_mut().fail_flush = false;
    let root = fs.root();
    assert!(fs.lookup(root, b"small").is_ok(), "so the name is back");
}

#[test]
fn a_reclaim_whose_publication_of_a_batched_unlink_faults_reports_it() {
    let mut fs = batched_unlink_behind(spanning_delete_volume(), &[b"frag"]);
    // The companion is written first, so faulting it leaves the slot a mount
    // reads untouched: the batch is certainly unpublished.
    let slot = slot_block(fs.ring_pos % RING_SLOTS);
    fs.block_mut()
        .write_faults
        .insert(ARXFS::<MemBlock>::companion(slot));
    assert!(
        fs.finish_reclaim().is_err(),
        "the batch carrying the unlink never published"
    );
    assert!(fs.read_only, "its publication failed, so the handle froze");
    let root = fs.root();
    assert!(
        fs.lookup(root, b"small").is_ok(),
        "and names the node again"
    );
    let bytes = fs.into_block().expect("a frozen handle closes").bytes();
    let mut fs =
        ARXFS::open(MemBlock::from_bytes(bytes, 512, 1 << 14), TEST_KEYING).expect("remount");
    let root = fs.root();
    assert!(
        fs.lookup(root, b"small").is_ok(),
        "as the volume does, so the failure reported is the truth"
    );
}

#[test]
fn a_reclaim_that_fails_after_publishing_a_batched_unlink_leaves_it_standing() {
    // A second, larger node pending, so the drain still has a transaction to
    // publish after the one that publishes the batch.
    let fixture = || {
        let mut fs = spanning_delete_volume();
        let root = fs.root();
        fs.create(root, b"frag2", NodeKind::RegularFile)
            .expect("create");
        fragment(&mut fs, b"frag2", 2 * SPANNING_EXTENTS);
        fs.flush().expect("publish");
        batched_unlink_behind(fs, &[b"frag", b"frag2"])
    };
    // Measure, on a twin, the writes the drain's first step takes, so the
    // device can refuse exactly the ones after it.
    let mut twin = fixture();
    let first = twin
        .next_pending_delete(0)
        .expect("read the set")
        .expect("a node is pending");
    let generation = twin.generation;
    let before = twin.block_mut().writes;
    twin.reclaim_step(first).expect("the first step");
    assert_ne!(twin.generation, generation, "which publishes the batch");
    let first_step = twin.block_mut().writes - before;
    let mut later_publishes = false;
    while let Some(ino) = twin.next_pending_delete(0).expect("read the set") {
        let generation = twin.generation;
        twin.reclaim_step(ino).expect("a later step");
        if twin.generation != generation {
            later_publishes = true;
            break;
        }
    }
    assert!(
        later_publishes,
        "a later step publishes too, so it meets the refusing device"
    );

    let mut fs = fixture();
    assert_eq!(fs.next_pending_delete(0), Ok(Some(first)));
    let writes = fs.block_mut().writes;
    fs.block_mut().write_fault_after = Some(writes + first_step);
    let aborts = fs.aborts;
    assert_eq!(
        fs.finish_reclaim(),
        Ok(()),
        "the batch carrying the unlink was published"
    );
    assert_ne!(
        fs.aborts, aborts,
        "before a later step's transaction was abandoned"
    );
    let root = fs.root();
    assert_eq!(fs.lookup(root, b"small"), Err(DriverError::NotFound));
    assert!(
        !pending_deletes(&mut fs).is_empty(),
        "the rest stays pending for the next drain"
    );
}

#[test]
fn the_host_hears_the_volume_enter_and_leave_its_reserve() {
    let (mut fs, host) = fmt_batched_with_host(512, 1 << 12, 64);
    assert_eq!(host.in_reserve(), None);
    fill(&mut fs);
    fs.flush().expect("publish");
    assert_eq!(host.in_reserve(), Some(true));
    let root = fs.root();
    for file in 0..16 {
        let name = alloc::format!("fill{file}");
        fs.remove(root, name.as_bytes()).expect("delete");
    }
    fs.flush().expect("publish");
    assert_eq!(host.in_reserve(), Some(false));
}

#[test]
fn a_volume_below_its_reserve_publishes_a_release_transaction_at_a_time() {
    let (mut fs, _host) = fmt_batched_with_host(512, 1 << 12, 64);
    fill(&mut fs);
    fs.flush().expect("publish");
    let generation = fs.generation;
    let threshold = fs.release_commit_blocks();
    let root = fs.root();
    fs.set_allocation_class(AllocationClass::System);
    fs.create(root, b"journal", NodeKind::RegularFile)
        .expect("create");
    let bytes = threshold * fs.data_capacity();
    // Incompressible and unrepeated, so every block is stored as written.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let body: Vec<u8> = (0..bytes)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect();
    let mut at = 0;
    while at < body.len() {
        at += fs
            .write_at(root, b"journal", at as u64, &body[at..])
            .expect("the system writes a release transaction's worth");
    }
    fs.set_allocation_class(AllocationClass::Ordinary);
    assert!(
        fs.generation > generation,
        "below the reserve a transaction that allocated {threshold} blocks is \
         published at once, even though its window has not expired"
    );
}

#[test]
fn the_reserve_is_chosen_again_on_every_mount() {
    let mut fs = fmt(512, 1 << 12, 64);
    fill(&mut fs);
    fs.flush().expect("publish");
    let bytes = fs.into_block().expect("the volume closes").bytes();
    let mut fs =
        ARXFS::open(MemBlock::from_bytes(bytes, 512, 1 << 12), TEST_KEYING).expect("remount");
    assert_eq!(fs.reserve_blocks(), release_share_blocks(512, 1 << 12));
    assert_eq!(reserve_held(&fs), fs.reserve_blocks());
    assert!(
        fs.in_reserve(),
        "the remounted volume is still down to its reserve"
    );
    let root = fs.root();
    fs.set_allocation_class(AllocationClass::System);
    fs.create(root, b"log", NodeKind::RegularFile)
        .expect("the system still writes after a remount");
}

#[test]
fn a_volume_holding_the_system_reserve_holds_it_over_its_write_set() {
    let write_set = [64 * 1024u64; 8];
    let mut fs = fmt(512, blocks(64 * MIB), 64)
        .with_system_reserve(&write_set)
        .expect("hold the system reserve");
    let target = system_reserve_blocks(512, blocks(64 * MIB), &write_set);
    assert_eq!(fs.reserve_blocks(), target);
    assert!(target > release_share_blocks(512, blocks(64 * MIB)));
    assert_eq!(reserve_held(&fs), target);
    let stats = fs.stats().expect("stats");
    assert_eq!(stats.avail_blocks, stats.free_blocks - target);
    let root = fs.root();
    fs.create(root, b"user", NodeKind::RegularFile)
        .expect("ordinary space remains");
}

#[test]
fn a_grown_volume_holds_the_reserve_its_new_size_calls_for() {
    let mut fs = fmt(512, 1 << 11, 64);
    assert_eq!(fs.reserve_blocks(), release_share_blocks(512, 1 << 11));
    fs.block_mut().enlarge_to(1 << 13);
    fs.grow().expect("grow");
    assert_eq!(fs.reserve_blocks(), release_share_blocks(512, 1 << 13));
    assert_eq!(reserve_held(&fs), fs.reserve_blocks());
}

#[test]
fn the_release_share_is_one_release_transaction_within_an_eighth_of_the_volume() {
    // A small volume holds an eighth of itself.
    assert_eq!(release_share_blocks(512, blocks(MIB)), blocks(MIB) / 8);
    // A large one holds one release transaction, its crossing operation and
    // the commit that publishes it, however large it grows.
    let share = release_share_blocks(512, blocks(4 << 30));
    assert_eq!(share, release_share_blocks(512, blocks(100 << 40)));
    assert!(share >= blocks(crate::reserve::RELEASE_TXN_BYTES));
    assert_eq!(share % 2, 0, "held in whole pairs");
}

#[test]
fn the_system_reserve_follows_its_size_policy() {
    let set = [64 * 1024u64; 16];
    let at = |bytes: u64| system_reserve_blocks(512, blocks(bytes), &set);
    // An eighth of a volume too small for the floor.
    assert_eq!(at(MIB), blocks(MIB) / 8);
    // The runway on a large volume, however much larger it grows.
    assert_eq!(at(4 << 30), blocks(128 * MIB));
    assert_eq!(at(100 << 40), blocks(128 * MIB));
    // Between, the floor over the write set, until a thirty-second of the
    // volume outgrows it.
    let floor = at(256 * MIB);
    assert!(
        floor > blocks(256 * MIB) / 32,
        "the floor dominates at 256 MiB"
    );
    assert_eq!(
        at(256 * MIB),
        at(200 * MIB),
        "and does not grow with the volume"
    );
    assert_eq!(at(2 << 30), blocks(2 << 30) / 32);
    // A larger write set moves the floor.
    let larger = [64 * 1024u64; 32];
    assert!(system_reserve_blocks(512, blocks(256 * MIB), &larger) > floor);
}

/// A truncate that extends a file frees nothing, so it does not draw as a
/// release — otherwise a user on a full volume could keep the reserve lent out
/// by growing and shrinking one file.
#[test]
fn only_a_truncate_that_drops_data_draws_as_a_release() {
    let mut fs = fmt(512, 1 << 12, 64);
    let root = fs.root();
    fs.create(root, b"file", NodeKind::RegularFile)
        .expect("create");
    fs.write_at(root, b"file", 0, &[2u8; 2048]).expect("write");
    let ino = file_ino(&mut fs, b"file");
    for (size, draw) in [(1 << 20, Draw::Ordinary), (0, Draw::Release)] {
        fs.draw = Draw::Release;
        fs.begin().expect("begin");
        fs.truncate_step(ino, size).expect("truncate");
        assert_eq!(fs.draw, draw, "a truncate to {size} bytes");
        fs.draw = Draw::Ordinary;
    }
}

/// Dropping one of a node's several names frees nothing, so it does not draw
/// as a release; the last name's removal does.
#[test]
fn only_the_last_unlink_draws_as_a_release() {
    let mut fs = fmt(512, 1 << 12, 64);
    let root = fs.root();
    fs.create(root, b"one", NodeKind::RegularFile)
        .expect("create");
    let node = fs.lookup(root, b"one").expect("lookup");
    fs.link(root, b"two", node).expect("a second name");
    for (name, draw) in [(&b"two"[..], Draw::Ordinary), (b"one", Draw::Release)] {
        fs.draw = Draw::Release;
        fs.begin().expect("begin");
        fs.remove_inner(root, name).expect("unlink");
        assert_eq!(fs.draw, draw);
        fs.draw = Draw::Ordinary;
    }
}
