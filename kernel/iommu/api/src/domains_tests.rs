extern crate std;

use super::*;
use crate::hostmem::HostFrames;
use crate::model::{ModelFormat, REACH};
use crate::queue::CommandQueue;
use crate::testunit::{completion, Clock0, Gated, Leaping};
use crate::{Access, TableMemory, IO_PAGE_SIZE};

fn table(frames: &HostFrames) -> IoPageTable<'_, ModelFormat> {
    IoPageTable::new(ModelFormat, 3, TableMemory::new(frames, None), REACH).unwrap()
}

/// A domain whose one page was mapped and unmapped, so its walk's tables
/// wait for a sync.
fn retiring(frames: &HostFrames) -> IoPageTable<'_, ModelFormat> {
    let mut table = table(frames);
    table
        .map(0x40_0000, 0x8000_0000, IO_PAGE_SIZE, Access::READ_WRITE)
        .unwrap();
    table.unmap(0x40_0000, IO_PAGE_SIZE).unwrap();
    assert!(table.has_retired());
    table
}

#[test]
fn a_domain_is_reached_by_its_id_alone() {
    let frames = HostFrames::new(0x1_0000_0000);
    let domains = DomainMap::new();
    assert!(domains.insert(1, table(&frames)).is_ok());
    assert!(domains.insert(2, table(&frames)).is_ok());
    assert!(domains.with(1, |_| Ok(())).is_ok());
    assert_eq!(domains.with(3, |_| Ok(())), Err(IommuError::OutOfRange));
    assert!(domains.remove(1).is_some());
    assert!(!domains.contains(1));
    assert!(domains.contains(2));
}

/// A refused insert hands the domain back, so its caller can free what the
/// map never took.
#[test]
fn a_refused_domain_comes_back_to_its_caller() {
    let frames = HostFrames::new(0x1_0000_0000);
    let domains = DomainMap::new();
    assert!(domains.insert(1, table(&frames)).is_ok());
    let live = frames.live();
    let (err, refused) = domains.insert(1, table(&frames)).unwrap_err();
    assert_eq!(err, IommuError::OutOfRange);
    drop(refused);
    assert_eq!(frames.live(), live);
    assert!(domains.contains(1));
}

/// A confirmed batch frees the tables it reached; one that failed frees
/// nothing and hands its tables to the next.
#[test]
fn a_confirmation_frees_the_tables_only_once_its_batch_is_done() {
    let frames = HostFrames::new(0x1_0000_0000);
    let memory = TableMemory::new(&frames, None);
    let queue = CommandQueue::new(&memory).unwrap();
    let live = frames.live();
    let domains = DomainMap::new();
    assert!(domains.insert(1, retiring(&frames)).is_ok());
    let clock = Leaping::new();
    let silent = Gated::new(&frames, &queue, true);
    let unit = |regs| Invalidator {
        queue: &queue,
        memory,
        clock: &clock,
        regs,
        completion,
    };
    assert_eq!(
        domains.confirm(1, |table| table, &unit(&silent), |_| Ok(([[1, 0]], None))),
        Err(IommuError::Unconfirmed)
    );
    assert!(domains.with(1, |table| Ok(table.has_retired())).unwrap());
    silent.release();
    let running = Gated::new(&frames, &queue, false);
    running.release();
    domains
        .confirm(1, |table| table, &unit(&running), |_| Ok(([[2, 0]], None)))
        .unwrap();
    assert!(!domains.with(1, |table| Ok(table.has_retired())).unwrap());
    drop(domains);
    assert_eq!(frames.live(), live, "every table went back");
}

/// Whether neither the map nor domain `id` is held by anyone.
fn unheld<V>(domains: &DomainMap<V>, id: u32) -> bool {
    domains.domains.try_write().is_some_and(|map| {
        map.get(&id)
            .is_some_and(|domain| domain.try_lock().is_some())
    })
}

/// Confirm domain 1 through `commands` on a unit that runs nothing until the
/// test releases it, checking that the confirmation, once it waits, holds
/// neither the map nor the domain.
fn confirm_while_held(commands: usize) {
    let frames = HostFrames::new(0x1_0000_0000);
    let memory = TableMemory::new(&frames, None);
    let queue = CommandQueue::new(&memory).unwrap();
    let domains = DomainMap::new();
    assert!(domains.insert(1, retiring(&frames)).is_ok());
    let clock = Clock0::new();
    let gate = Gated::new(&frames, &queue, true);
    std::thread::scope(|scope| {
        let reads = clock.reads();
        let waiting = scope.spawn(|| {
            let unit = Invalidator {
                queue: &queue,
                memory,
                clock: &clock,
                regs: &gate,
                completion,
            };
            let batch = core::iter::repeat_n([1, 0], commands);
            domains.confirm(1, |table| table, &unit, |_| Ok((batch, None)))
        });
        while clock.reads() == reads {
            core::hint::spin_loop();
        }
        assert!(unheld(&domains, 1));
        gate.release();
        assert_eq!(waiting.join().unwrap(), Ok(()));
    });
    assert!(!domains.with(1, |table| Ok(table.has_retired())).unwrap());
}

/// A confirmation waiting on its unit holds no domain's lock, its own
/// included, nor the map's.
#[test]
fn a_waiting_confirmation_holds_no_domain() {
    confirm_while_held(1);
}

/// Nor does one whose batch waits on the unit for room in the ring.
#[test]
fn a_confirmation_waiting_for_room_holds_no_domain() {
    confirm_while_held(CommandQueue::SLOTS);
}
