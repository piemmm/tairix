extern crate std;

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use super::*;
use crate::hostmem::HostFrames;
use crate::testunit::{completion, Clock0, Leaping};

/// A unit that consumes the ring up to the tail the moment it is set and
/// stores each completion's token, or stops on a command naming `reject`
/// until resumed.
struct Unit<'m> {
    frames: &'m HostFrames,
    ring: u64,
    status: u64,
    head: Cell<usize>,
    tail: Cell<usize>,
    head_reads: Cell<usize>,
    reject: u64,
    stopped: Cell<bool>,
}

impl Unit<'_> {
    fn consume(&self) -> Result<(), IommuError> {
        while !self.stopped.get() && self.head.get() != self.tail.get() {
            let slot = self.head.get();
            let word = self
                .frames
                .word(self.ring + 16 * slot as u64)
                .ok_or(IommuError::Hardware)?;
            if word == self.reject {
                self.stopped.set(true);
                return Ok(());
            }
            if word >> 60 == 0xC {
                self.frames.store_word(self.status, word & 0xFFFF_FFFF);
            }
            self.head.set((slot + 1) % SLOTS);
        }
        Ok(())
    }
}

impl QueueRegisters for Unit<'_> {
    fn head(&self) -> Result<usize, IommuError> {
        self.head_reads.set(self.head_reads.get() + 1);
        Ok(self.head.get())
    }

    fn set_tail(&self, tail: usize) -> Result<(), IommuError> {
        self.tail.set(tail);
        self.consume()
    }

    fn stopped_at(&self) -> Result<Option<usize>, IommuError> {
        Ok(self.stopped.get().then(|| self.head.get()))
    }

    fn resume(
        &self,
        queue: &CommandQueue,
        memory: &TableMemory<'_>,
        slot: usize,
    ) -> Result<(), IommuError> {
        queue.replace(memory, slot, [0, 0])?;
        self.stopped.set(false);
        self.consume()
    }
}

fn rig<'m>(frames: &'m HostFrames, queue: &CommandQueue, reject: u64) -> Unit<'m> {
    Unit {
        frames,
        ring: queue.ring(),
        status: queue.status_word(),
        head: Cell::new(0),
        tail: Cell::new(0),
        head_reads: Cell::new(0),
        reject,
        stopped: Cell::new(false),
    }
}

#[test]
fn a_batch_returns_once_its_completion_is_stored_and_wraps_the_ring() {
    let frames = HostFrames::new(0x1_0000_0000);
    let memory = TableMemory::new(&frames, None);
    let queue = CommandQueue::new(&memory).unwrap();
    let unit = rig(&frames, &queue, u64::MAX);
    let clock = Clock0::new();
    for round in 0..(2 * SLOTS) {
        queue
            .run(&memory, &clock, &unit, [[round as u64, 0]], completion)
            .unwrap();
    }
    assert_eq!(
        unit.head.get(),
        queue.state.lock().tail,
        "the unit consumed every slot"
    );
}

/// A stream longer than the ring is handed over as the unit makes room, with
/// the head register read only when the ring looks full.
#[test]
fn a_stream_longer_than_the_ring_is_handed_over_as_room_frees() {
    let frames = HostFrames::new(0x1_0000_0000);
    let memory = TableMemory::new(&frames, None);
    let queue = CommandQueue::new(&memory).unwrap();
    let unit = rig(&frames, &queue, u64::MAX);
    let clock = Clock0::new();
    let stream = (0..3 * SLOTS as u64).map(|command| [command, 0]);
    queue
        .run(&memory, &clock, &unit, stream, completion)
        .unwrap();
    assert_eq!(
        unit.head.get(),
        queue.state.lock().tail,
        "the unit consumed every command"
    );
    assert!(
        unit.head_reads.get() < SLOTS,
        "the head was read {} times for {} commands",
        unit.head_reads.get(),
        3 * SLOTS
    );
}

#[test]
fn a_rejected_command_fails_its_batch_and_the_queue_runs_on() {
    let frames = HostFrames::new(0x1_0000_0000);
    let memory = TableMemory::new(&frames, None);
    let queue = CommandQueue::new(&memory).unwrap();
    let unit = rig(&frames, &queue, 0xBAD);
    let clock = Clock0::new();
    assert_eq!(
        queue.run(&memory, &clock, &unit, [[0xBAD, 0]], completion),
        Err(IommuError::Hardware)
    );
    queue
        .run(&memory, &clock, &unit, [[1, 0]], completion)
        .unwrap();
    assert!(
        queue.state.lock().rejected.is_empty(),
        "the charge left with its batch's answer"
    );
}

/// A rejection stops the unit for every batch behind it, but only the batch
/// holding the rejected command fails, whichever is waited on first.
#[test]
fn a_rejection_is_charged_to_the_batch_holding_it_alone() {
    for rejected_first in [false, true] {
        let frames = HostFrames::new(0x1_0000_0000);
        let memory = TableMemory::new(&frames, None);
        let queue = CommandQueue::new(&memory).unwrap();
        let unit = rig(&frames, &queue, 0xBAD);
        let clock = Clock0::new();
        let bad = queue
            .submit(&memory, &clock, &unit, [[1, 0], [0xBAD, 0]], completion)
            .unwrap();
        let good = queue
            .submit(&memory, &clock, &unit, [[2, 0]], completion)
            .unwrap();
        assert!(
            unit.stopped.get(),
            "the unit stopped on the rejected command"
        );
        let (bad, good) = if rejected_first {
            let bad = queue.wait(&memory, &clock, &unit, bad);
            (bad, queue.wait(&memory, &clock, &unit, good))
        } else {
            let good = queue.wait(&memory, &clock, &unit, good);
            (queue.wait(&memory, &clock, &unit, bad), good)
        };
        assert_eq!(
            bad,
            Err(IommuError::Hardware),
            "rejected first: {rejected_first}"
        );
        assert_eq!(good, Ok(()), "rejected first: {rejected_first}");
    }
}

/// A unit confirming by consumption, which stores nothing; it stops consuming
/// before slot `stall` where one is given.
struct Consuming {
    head: Cell<usize>,
    stall: Option<usize>,
}

impl QueueRegisters for Consuming {
    fn head(&self) -> Result<usize, IommuError> {
        Ok(self.head.get())
    }

    fn set_tail(&self, tail: usize) -> Result<(), IommuError> {
        while self.head.get() != tail && Some(self.head.get()) != self.stall {
            self.head.set((self.head.get() + 1) % SLOTS);
        }
        Ok(())
    }

    fn stopped_at(&self) -> Result<Option<usize>, IommuError> {
        Ok(None)
    }

    fn resume(
        &self,
        _queue: &CommandQueue,
        _memory: &TableMemory<'_>,
        _slot: usize,
    ) -> Result<(), IommuError> {
        Ok(())
    }

    fn completion(&self) -> Completion {
        Completion::Consumed
    }
}

/// A batch a consuming unit has moved its head past is done; one it stops
/// short of, with the completion still ahead of its head, is not.
#[test]
fn a_unit_confirming_by_consumption_is_done_once_its_head_passes_the_batch() {
    let frames = HostFrames::new(0x1_0000_0000);
    let memory = TableMemory::new(&frames, None);
    let queue = CommandQueue::new(&memory).unwrap();
    let clock = Clock0::new();
    let unit = Consuming {
        head: Cell::new(0),
        stall: None,
    };
    for round in 0..(2 * SLOTS) {
        queue
            .run(&memory, &clock, &unit, [[round as u64, 0]], completion)
            .unwrap();
    }
    let tail = queue.state.lock().tail;
    assert_eq!(unit.head.get(), tail);
    let stuck = Consuming {
        head: Cell::new(tail),
        stall: Some((tail + 1) % SLOTS),
    };
    assert_eq!(
        queue.run(&memory, &Leaping::new(), &stuck, [[1, 0]], completion),
        Err(IommuError::Unconfirmed),
        "the completion is never consumed"
    );
}

/// A unit that never stores its completion is not waited on forever.
#[test]
fn a_unit_that_never_completes_is_unconfirmed() {
    struct Silent;
    impl QueueRegisters for Silent {
        fn head(&self) -> Result<usize, IommuError> {
            Ok(0)
        }
        fn set_tail(&self, _tail: usize) -> Result<(), IommuError> {
            Ok(())
        }
        fn stopped_at(&self) -> Result<Option<usize>, IommuError> {
            Ok(None)
        }
        fn resume(
            &self,
            _queue: &CommandQueue,
            _memory: &TableMemory<'_>,
            _slot: usize,
        ) -> Result<(), IommuError> {
            Ok(())
        }
    }
    let frames = HostFrames::new(0x1_0000_0000);
    let memory = TableMemory::new(&frames, None);
    let queue = CommandQueue::new(&memory).unwrap();
    assert_eq!(
        queue.run(&memory, &Leaping::new(), &Silent, [[1, 0]], completion),
        Err(IommuError::Unconfirmed)
    );
}

/// A batch that completed while its waiter's CPU was taken away past the
/// deadline is confirmed: only a poll begun after the deadline fails a wait.
#[test]
fn a_completion_landing_while_the_waiter_was_away_is_confirmed() {
    /// Its second read finds the deadline long past, the batch having landed
    /// meanwhile.
    struct Away {
        reads: AtomicU32,
        landed: AtomicBool,
    }
    impl Clock for Away {
        fn now_ns(&self) -> u64 {
            if self.reads.fetch_add(1, Ordering::Relaxed) == 0 {
                return 0;
            }
            self.landed.store(true, Ordering::Relaxed);
            u64::MAX
        }
    }
    let clock = Away {
        reads: AtomicU32::new(0),
        landed: AtomicBool::new(false),
    };
    assert_eq!(
        wait_for(&clock, || Ok(clock.landed.load(Ordering::Relaxed))),
        Ok(())
    );
}

/// A command naming no submitter, which the live unit rejects.
const REJECT: u64 = 0xBAD;

/// A unit on a thread of its own, as hardware is: it runs the ring up to
/// whatever tail it was last handed, recording each command it runs and
/// storing each completion's token once every command before it ran, and
/// stops on [`REJECT`] until resumed. While `paused` it runs nothing.
struct Live<'m> {
    frames: &'m HostFrames,
    ring: u64,
    status: u64,
    head: AtomicUsize,
    tail: AtomicUsize,
    stopped: AtomicBool,
    paused: AtomicBool,
    quit: AtomicBool,
    /// Per submitter, the last round a command of its ran.
    ran: [AtomicU64; SUBMITTERS],
    /// Commands run, the unit's own measure of time.
    worked: AtomicU64,
}

const SUBMITTERS: usize = 4;

impl Live<'_> {
    fn serve(&self) {
        while !self.quit.load(Ordering::Acquire) {
            let head = self.head.load(Ordering::Relaxed);
            if self.stopped.load(Ordering::Acquire)
                || self.paused.load(Ordering::Acquire)
                || head == self.tail.load(Ordering::Acquire)
            {
                core::hint::spin_loop();
                continue;
            }
            let Some(word) = self.frames.word(self.ring + 16 * head as u64) else {
                return;
            };
            if word == REJECT {
                self.stopped.store(true, Ordering::Release);
                continue;
            }
            if word >> 60 == 0xC {
                // Every command before a completion is done before its token
                // lands.
                core::sync::atomic::fence(Ordering::Release);
                self.frames.store_word(self.status, word & 0xFFFF_FFFF);
            } else if word >> 60 == 0xA {
                let submitter = (word >> 32) as usize & 0xFF;
                self.ran[submitter].store(word & 0xFFFF_FFFF, Ordering::Relaxed);
            }
            self.head.store((head + 1) % SLOTS, Ordering::Release);
            self.worked.fetch_add(1, Ordering::Release);
        }
    }
}

impl QueueRegisters for Live<'_> {
    fn head(&self) -> Result<usize, IommuError> {
        Ok(self.head.load(Ordering::Acquire))
    }

    fn set_tail(&self, tail: usize) -> Result<(), IommuError> {
        self.tail.store(tail, Ordering::Release);
        Ok(())
    }

    fn stopped_at(&self) -> Result<Option<usize>, IommuError> {
        Ok(self
            .stopped
            .load(Ordering::Acquire)
            .then(|| self.head.load(Ordering::Acquire)))
    }

    fn resume(
        &self,
        queue: &CommandQueue,
        memory: &TableMemory<'_>,
        slot: usize,
    ) -> Result<(), IommuError> {
        queue.replace(memory, slot, [0, 0])?;
        self.stopped.store(false, Ordering::Release);
        Ok(())
    }
}

/// Stops a [`Live`] unit when dropped, so a failed assertion ends its test
/// rather than leaving the unit's thread for the scope to wait on.
struct Quits<'a>(&'a AtomicBool);

impl Drop for Quits<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// A [`Live`] unit's time: one step per command it runs, so a wait expires
/// only once the unit has run a hundred rings past it unanswered, however the
/// host schedules the threads. Counts the reads it answered.
struct UnitTime<'a, 'm> {
    unit: &'a Live<'m>,
    reads: AtomicU32,
}

impl<'a, 'm> UnitTime<'a, 'm> {
    const STEP_NS: u64 = COMMAND_BUDGET_NS / (100 * SLOTS as u64);

    fn of(unit: &'a Live<'m>) -> Self {
        Self {
            unit,
            reads: AtomicU32::new(0),
        }
    }
}

impl Clock for UnitTime<'_, '_> {
    fn now_ns(&self) -> u64 {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.unit
            .worked
            .load(Ordering::Acquire)
            .saturating_mul(Self::STEP_NS)
    }
}

fn live<'m>(frames: &'m HostFrames, queue: &CommandQueue) -> Live<'m> {
    Live {
        frames,
        ring: queue.ring(),
        status: queue.status_word(),
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
        stopped: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        quit: AtomicBool::new(false),
        ran: [const { AtomicU64::new(0) }; SUBMITTERS],
        worked: AtomicU64::new(0),
    }
}

/// A command submitter `submitter` sends in round `round`.
fn marked(submitter: usize, round: u64) -> Command {
    [0xA << 60 | (submitter as u64) << 32 | round, 0]
}

/// Submitters on several threads share one queue with a unit running on
/// another: every wait answers only once its own batch ran — never early,
/// though later batches complete over it — and a rejected command fails its
/// own batch and no other.
#[test]
fn concurrent_batches_each_wait_for_their_own_completion() {
    // Miri interprets every spin, so it proves the protocol over fewer.
    const ROUNDS: u64 = if cfg!(miri) { 24 } else { 20_000 };
    let frames = HostFrames::new(0x1_0000_0000);
    let memory = TableMemory::new(&frames, None);
    let queue = CommandQueue::new(&memory).unwrap();
    let unit = live(&frames, &queue);
    let clock = UnitTime::of(&unit);
    std::thread::scope(|scope| {
        scope.spawn(|| unit.serve());
        let _quits = Quits(&unit.quit);
        let submitters: std::vec::Vec<_> = (0..SUBMITTERS)
            .map(|submitter| {
                let (memory, queue, unit, clock) = (&memory, &queue, &unit, &clock);
                scope.spawn(move || {
                    for round in 1..=ROUNDS {
                        let rejected = round % 11 == submitter as u64;
                        let mut batch = std::vec![marked(submitter, round)];
                        if rejected {
                            batch.push([REJECT, 0]);
                        }
                        // Now and then one longer than the ring, handed over
                        // as room frees while the others wait.
                        if round % 23 == 0 {
                            batch.extend((0..SLOTS + 44).map(|_| marked(submitter, round)));
                        }
                        let answer = queue.run(memory, clock, unit, batch, completion);
                        if rejected {
                            assert_eq!(answer, Err(IommuError::Hardware), "round {round}");
                        } else {
                            assert_eq!(answer, Ok(()), "submitter {submitter} round {round}");
                        }
                        assert!(
                            unit.ran[submitter].load(Ordering::Relaxed) >= round,
                            "submitter {submitter} answered before round {round} ran"
                        );
                    }
                })
            })
            .collect();
        for submitter in submitters {
            submitter.join().unwrap();
        }
    });
    assert!(
        queue.state.lock().rejected.is_empty(),
        "every charge was answered"
    );
}

/// A batch waiting on its completion holds no lock: another submits past it
/// while the unit runs nothing, and both are answered once it does.
#[test]
fn a_waiting_batch_holds_no_lock_another_needs() {
    let frames = HostFrames::new(0x1_0000_0000);
    let memory = TableMemory::new(&frames, None);
    let queue = CommandQueue::new(&memory).unwrap();
    let unit = live(&frames, &queue);
    unit.paused.store(true, Ordering::Release);
    let clock = UnitTime::of(&unit);
    std::thread::scope(|scope| {
        scope.spawn(|| unit.serve());
        let _quits = Quits(&unit.quit);
        let first = queue
            .submit(&memory, &clock, &unit, [marked(0, 1)], completion)
            .unwrap();
        let reads = clock.reads.load(Ordering::Relaxed);
        let waiter = scope.spawn(|| queue.wait(&memory, &clock, &unit, first));
        while clock.reads.load(Ordering::Relaxed) == reads {
            core::hint::spin_loop();
        }
        let second = queue
            .submit(&memory, &clock, &unit, [marked(1, 1)], completion)
            .unwrap();
        unit.paused.store(false, Ordering::Release);
        assert_eq!(queue.wait(&memory, &clock, &unit, second), Ok(()));
        assert_eq!(waiter.join().unwrap(), Ok(()));
    });
}
