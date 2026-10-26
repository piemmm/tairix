use super::*;

use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{FilesystemWrite, Unpinned};

use crate::fs::memfs::{RwMockFs, ADMIN_RECORD};
use crate::sleeplock::SleepLock;
use crate::test_alloc::{
    opt_in_current_thread, opt_out_current_thread, refuse_allocations_on_current_thread, LiveBytes,
};

/// Whether every outstanding pin still has its release's slot.
fn reserved(volume: &VolumeNodes) -> bool {
    let queue = volume.queue.lock();
    queue.outstanding + queue.pending.len() <= queue.pending.capacity()
}

/// A pin of node `raw` on `volume`, its slot admitted first.
fn pin(volume: &Arc<VolumeNodes>, raw: u64) -> NodeRef {
    let slot = volume.admit().expect("admits");
    NodeRef::new(slot, NodeId::from_raw(raw), NodeKind::RegularFile).expect("allocates")
}

/// How many releases wait for a reap.
fn queued(volume: &VolumeNodes) -> usize {
    volume.queue.lock().pending.len()
}

/// Spend `slot` as [`NodeRef::new`] does, for a test that releases through
/// [`VolumeNodes::queue_release`] itself.
fn consumed(mut slot: ReleaseSlot) {
    slot.taken = true;
}

#[test]
fn a_release_allocates_nothing() {
    static COUNTER: LiveBytes = LiveBytes::new();
    let volume = Arc::new(VolumeNodes::new(1));
    let held: Vec<NodeRef> = (1..=64).map(|raw| pin(&volume, raw)).collect();
    opt_in_current_thread(&COUNTER);
    drop(held);
    opt_out_current_thread();
    assert_eq!(COUNTER.allocations(), 0, "a release allocated");
    assert_eq!(volume.outstanding(), 0);
    assert_eq!(queued(&volume), 64);
}

#[test]
fn an_admission_the_heap_refuses_reserves_nothing() {
    let volume = Arc::new(VolumeNodes::new(2));
    refuse_allocations_on_current_thread(true);
    let refused = volume.admit();
    refuse_allocations_on_current_thread(false);
    assert!(matches!(refused, Err(VfsError::OutOfMemory)));
    assert_eq!(volume.outstanding(), 0);
}

/// A slot the driver's refusal left unused gives itself back, releasing
/// nothing.
#[test]
fn a_slot_dropped_unconsumed_gives_itself_back() {
    let volume = Arc::new(VolumeNodes::new(3));
    let slot = volume.admit().expect("admits");
    assert_eq!(volume.outstanding(), 1);
    drop(slot);
    assert_eq!(volume.outstanding(), 0);
    assert!(reserved(&volume));
    assert!(!volume.take_reap_due(), "nothing was released");
}

/// A slot a pin consumed stays reserved for the pin's whole life and is spent
/// by its release, never given back as well.
#[test]
fn a_consumed_slot_is_spent_by_its_pins_release() {
    let volume = Arc::new(VolumeNodes::new(12));
    let held = pin(&volume, 1);
    let shared = held.clone();
    assert_eq!(volume.outstanding(), 1);
    drop(held);
    assert_eq!((volume.outstanding(), queued(&volume)), (1, 0));
    drop(shared);
    assert_eq!((volume.outstanding(), queued(&volume)), (0, 1));
    assert!(reserved(&volume));
}

#[test]
fn a_release_marks_the_registration_due_once() {
    let volume = Arc::new(VolumeNodes::new(4));
    drop(pin(&volume, 1));
    assert!(volume.take_reap_due());
    assert!(!volume.take_reap_due());
}

/// A registration reaped inline is due for the next operation on its volume,
/// and never wakes the flusher, which must not wait on its driver.
#[test]
fn a_release_reaped_inline_never_wakes_the_flusher() {
    let volume = Arc::new(VolumeNodes::reaped_inline(14));
    consumed(volume.admit().expect("admits"));
    assert!(!volume.queue_release(NodeId::from_raw(1)));
    assert!(volume.is_reap_due());
}

/// Files named `names`, created on `fs`, each pinned on `volume`.
fn pinned_files(volume: &Arc<VolumeNodes>, fs: &mut RwMockFs, names: &[&[u8]]) -> Vec<NodeRef> {
    let root = fs.root();
    names
        .iter()
        .map(|name| {
            let file = fs
                .create(root, name, NodeKind::RegularFile, &ADMIN_RECORD)
                .expect("create");
            let pinned = fs.pin(file).expect("pin");
            let slot = volume.admit().expect("admits");
            NodeRef::new(slot, pinned, NodeKind::RegularFile).expect("allocates")
        })
        .collect()
}

/// A release its driver answers busy — never sent — stays first in line and
/// the registration due, so the next operation hands every release over in
/// order, allocating nothing to keep it.
#[test]
fn a_release_answered_busy_waits_first_in_line_when_reaped_inline() {
    let volume = Arc::new(VolumeNodes::reaped_inline(15));
    let mut fs = RwMockFs::new();
    let held = pinned_files(&volume, &mut fs, &[b"a", b"b"]);
    let first = held[0].pinned();
    drop(held);
    fs.refuse_unpins(Some(DriverError::Busy));
    volume.reap_inline(&mut fs);
    assert_eq!(queued(&volume), 2, "neither was sent");
    assert!(volume.is_reap_due());
    assert!(reserved(&volume));
    fs.refuse_unpins(None);
    volume.reap_inline(&mut fs);
    assert_eq!(queued(&volume), 0);
    assert_eq!(fs.unpin(first), Err(DriverError::NotFound), "both went");
}

/// A registration the flusher reaps is left to it: an operation reaps
/// nothing, and the flusher's reap drops a release answered busy as it drops
/// every other answer.
#[test]
fn a_registration_the_flusher_reaps_is_left_to_it() {
    let volume = Arc::new(VolumeNodes::new(16));
    let mut fs = RwMockFs::new();
    drop(pinned_files(&volume, &mut fs, &[b"a"]));
    volume.reap_inline(&mut fs);
    assert_eq!(queued(&volume), 1);
    fs.refuse_unpins(Some(DriverError::Busy));
    assert!(volume.take_reap_due());
    volume.reap(&mut fs);
    assert_eq!(queued(&volume), 0);
    assert!(!volume.is_reap_due());
}

/// Only the release that makes a registration due wakes the flusher; the
/// releases after it, before the flusher's pass takes the registration, add
/// to the work that wake already asked for.
#[test]
fn a_burst_of_releases_wakes_the_flusher_once() {
    let volume = Arc::new(VolumeNodes::new(11));
    for _ in 0..3 {
        consumed(volume.admit().expect("admits"));
    }
    assert!(volume.queue_release(NodeId::from_raw(1)));
    assert!(!volume.queue_release(NodeId::from_raw(2)));
    assert!(volume.take_reap_due());
    assert!(
        volume.queue_release(NodeId::from_raw(3)),
        "a release after the pass took the registration wakes it again"
    );
    assert_eq!(queued(&volume), 3);
}

/// One step of the opener's or the closer's side of an interleaving.
#[derive(Copy, Clone)]
enum Step {
    /// Reserve a slot for a pin about to be taken.
    Admit,
    /// The driver answered: wrap the pin.
    Pin,
    /// The driver refused: drop the slot.
    Abandon,
    /// Drop the oldest reference.
    Release,
    /// A reap tells the driver of the queue.
    Reap,
}

/// Every order of `a`'s and `b`'s steps that keeps each side's own order.
fn interleavings(a: &[Step], b: &[Step]) -> Vec<Vec<Step>> {
    match (a.split_first(), b.split_first()) {
        (None, _) => alloc::vec![b.to_vec()],
        (_, None) => alloc::vec![a.to_vec()],
        (Some((&first, rest)), Some((&other, others))) => {
            let mut all = Vec::new();
            for mut tail in interleavings(rest, b) {
                tail.insert(0, first);
                all.push(tail);
            }
            for mut tail in interleavings(a, others) {
                tail.insert(0, other);
                all.push(tail);
            }
            all
        }
    }
}

/// Every interleaving of opens and closes keeps a slot for every pin and
/// loses no release. Each step is one hold of the queue's lock, so these are
/// all the orders the lock can serialise them in; loom cannot build this
/// crate (D131).
#[test]
fn every_interleaving_keeps_a_slot_for_every_pin() {
    let opener = [
        Step::Admit,
        Step::Pin,
        Step::Admit,
        Step::Abandon,
        Step::Admit,
        Step::Pin,
    ];
    let closer = [Step::Release, Step::Reap, Step::Release, Step::Reap];
    for schedule in interleavings(&opener, &closer) {
        let volume = Arc::new(VolumeNodes::new(5));
        let mut fs = RwMockFs::new();
        let mut held: Vec<NodeRef> = Vec::new();
        let mut taking: Vec<ReleaseSlot> = Vec::new();
        let (mut pinned, mut released) = (0u64, 0usize);
        for step in schedule {
            match step {
                Step::Admit => taking.push(volume.admit().expect("admits")),
                Step::Pin => {
                    pinned += 1;
                    let slot = taking.remove(0);
                    held.push(
                        NodeRef::new(slot, NodeId::from_raw(pinned), NodeKind::RegularFile)
                            .expect("allocates"),
                    );
                }
                Step::Abandon => drop(taking.remove(0)),
                Step::Release => {
                    if !held.is_empty() {
                        drop(held.remove(0));
                    }
                }
                Step::Reap => {
                    let before = queued(&volume);
                    volume.reap(&mut fs);
                    released += before - queued(&volume);
                }
            }
            assert!(reserved(&volume), "a pin lost its release's slot");
            assert_eq!(volume.outstanding(), held.len() + taking.len());
        }
        drop(held);
        released += queued(&volume);
        assert_eq!(released as u64, pinned, "a release was lost");
        assert_eq!(volume.outstanding(), 0);
    }
}

/// A release racing the write-back flusher's passes is never left queued
/// while the registration reads as reaped: a release queues before it marks
/// the registration due, and a pass clears the mark before it takes the
/// queue. Each round ends with both sides done, so a stranded release is seen
/// in the round that stranded it rather than swept up by a later wake. loom
/// cannot build this crate (D131), so the race runs on the host's own cores.
#[test]
fn a_release_racing_a_flusher_pass_is_never_stranded() {
    // The interpreter preempts at random points in every round, so it needs
    // far fewer rounds than hardware to reach the racing orders.
    const ROUNDS: u64 = if cfg!(miri) { 200 } else { 20_000 };
    let volume = Arc::new(VolumeNodes::new(13));
    let queued_round = Arc::new(core::sync::atomic::AtomicU64::new(0));
    let edges = Arc::new(std::sync::Barrier::new(3));
    let releaser = {
        let (volume, queued_round, edges) = (
            Arc::clone(&volume),
            Arc::clone(&queued_round),
            Arc::clone(&edges),
        );
        std::thread::spawn(move || {
            let mut woke = 0usize;
            for round in 1..=ROUNDS {
                edges.wait();
                woke += usize::from(volume.queue_release(NodeId::from_raw(round)));
                queued_round.store(round, Ordering::Release);
                edges.wait();
            }
            woke
        })
    };
    let flusher = {
        let (volume, queued_round, edges) = (
            Arc::clone(&volume),
            Arc::clone(&queued_round),
            Arc::clone(&edges),
        );
        std::thread::spawn(move || {
            let mut fs = RwMockFs::new();
            let mut passes = 0usize;
            for round in 1..=ROUNDS {
                edges.wait();
                loop {
                    let finished = queued_round.load(Ordering::Acquire) == round;
                    if volume.take_reap_due() {
                        passes += 1;
                        volume.reap(&mut fs);
                    }
                    if finished {
                        break;
                    }
                }
                edges.wait();
            }
            passes
        })
    };
    let mut fs = RwMockFs::new();
    let mut fell_due_after = 0usize;
    for round in 1..=ROUNDS {
        consumed(volume.admit().expect("admits"));
        edges.wait();
        edges.wait();
        let due = volume.take_reap_due();
        assert!(
            queued(&volume) == 0 || due,
            "round {round}: a release was left queued with the registration reaped"
        );
        volume.reap(&mut fs);
        fell_due_after += usize::from(due);
    }
    let woke = releaser.join().expect("the releaser completes");
    let passes = flusher.join().expect("the flusher completes");
    assert_eq!(
        woke,
        passes + fell_due_after,
        "one wake each time the registration fell due"
    );
    assert_eq!(volume.outstanding(), 0);
}

#[test]
fn a_reap_hands_the_driver_every_release() {
    let volume = Arc::new(VolumeNodes::new(6));
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let file = fs
        .create(root, b"f", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    let pinned = fs.pin(file).expect("pin");
    let slot = volume.admit().expect("admits");
    drop(NodeRef::new(slot, pinned, NodeKind::RegularFile).expect("allocates"));
    volume.reap(&mut fs);
    assert_eq!(
        fs.unpin(pinned),
        Err(tairix_abi::driver::DriverError::NotFound),
        "the reap released the driver's hold"
    );
    assert_eq!(queued(&volume), 0);
}

#[test]
fn a_reap_allocates_nothing() {
    static COUNTER: LiveBytes = LiveBytes::new();
    let volume = Arc::new(VolumeNodes::new(10));
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let mut held = Vec::new();
    for at in 0..64 {
        let name = alloc::format!("f{at}");
        let file = fs
            .create(root, name.as_bytes(), NodeKind::RegularFile, &ADMIN_RECORD)
            .expect("create");
        let pinned = fs.pin(file).expect("pin");
        let slot = volume.admit().expect("admits");
        held.push(NodeRef::new(slot, pinned, NodeKind::RegularFile).expect("allocates"));
    }
    let first = held[0].pinned();
    drop(held);
    opt_in_current_thread(&COUNTER);
    volume.reap(&mut fs);
    opt_out_current_thread();
    assert_eq!(COUNTER.allocations(), 0, "a reap allocated");
    assert_eq!(queued(&volume), 0);
    assert_eq!(
        fs.unpin(first),
        Err(tairix_abi::driver::DriverError::NotFound),
        "the reap released the driver's holds"
    );
}

/// Retiring under the driver's lock is seen by whoever takes the lock next,
/// a release after it still queues, and its reap empties the queue without
/// telling the driver, which let every pin go before it was retired.
#[test]
fn a_release_after_retirement_queues_and_its_reap_tells_the_driver_nothing() {
    let volume = Arc::new(VolumeNodes::new(7));
    let mut fs = RwMockFs::new();
    let root = fs.root();
    let file = fs
        .create(root, b"f", NodeKind::RegularFile, &ADMIN_RECORD)
        .expect("create");
    let pinned = fs.pin(file).expect("pin");
    let slot = volume.admit().expect("admits");
    let held = NodeRef::new(slot, pinned, NodeKind::RegularFile).expect("allocates");
    let driver = Arc::new(SleepLock::new(fs));
    let retiring = {
        let (driver, volume) = (Arc::clone(&driver), Arc::clone(&volume));
        std::thread::spawn(move || {
            let _held = driver.lock();
            volume.retire();
        })
    };
    retiring.join().expect("retires");
    let mut fs = driver.lock();
    assert!(volume.is_retired());
    drop(held);
    assert!(
        volume.take_reap_due(),
        "a release after retirement still queues"
    );
    volume.reap(&mut *fs);
    assert_eq!(queued(&volume), 0, "the reap emptied the queue");
    assert_eq!(
        fs.unpin(pinned),
        Ok(Unpinned::Released),
        "the driver was told nothing"
    );
}

#[test]
fn references_are_equal_when_they_name_one_node_of_one_registration() {
    let first = Arc::new(VolumeNodes::new(8));
    let second = Arc::new(VolumeNodes::new(9));
    let a = pin(&first, 3);
    let b = pin(&first, 3);
    let c = pin(&second, 3);
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(a.clone(), a);
    assert_eq!((a.instance(), a.pinned().raw()), (8, 3));
}
