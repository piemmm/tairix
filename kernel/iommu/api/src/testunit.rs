//! Units the engine's own tests drive its queue through.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use tairix_sync::SpinLock;

use crate::hostmem::HostFrames;
use crate::memory::TableMemory;
use crate::queue::{Command, CommandQueue, QueueRegisters};
use crate::{Clock, IommuError};

/// A completion as the test units read it: its token in the low word.
pub(crate) fn completion(token: u32, _status: u64) -> Command {
    [0xC << 60 | u64::from(token), 0]
}

/// A clock a microsecond further on at every read.
pub(crate) struct Clock0(pub(crate) AtomicU64);

impl Clock0 {
    pub(crate) const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Reads taken so far.
    pub(crate) fn reads(&self) -> u64 {
        self.0.load(Ordering::Relaxed) / 1_000
    }
}

impl Clock for Clock0 {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(1_000, Ordering::Relaxed)
    }
}

/// A clock that runs out a command's budget in a few dozen reads, for a wait
/// that must give up: interpreted, a million polls would take an hour.
pub(crate) struct Leaping(AtomicU64);

impl Leaping {
    pub(crate) const fn new() -> Self {
        Self(AtomicU64::new(0))
    }
}

impl Clock for Leaping {
    fn now_ns(&self) -> u64 {
        self.0
            .fetch_add(crate::queue::COMMAND_BUDGET_NS / 64, Ordering::Relaxed)
    }
}

/// A unit that runs what it is handed at once, or, while gated, only once
/// [`Self::release`]d: it stores each completion's token.
pub(crate) struct Gated<'m> {
    frames: &'m HostFrames,
    ring: u64,
    status: u64,
    head: AtomicUsize,
    /// Changed and acted on together: apart, a release and a handover can each
    /// read the other's store stale and leave commands nothing runs.
    handover: SpinLock<Handover>,
}

/// The tail handed over, and whether the unit still holds what it is handed.
struct Handover {
    tail: usize,
    held: bool,
}

impl<'m> Gated<'m> {
    pub(crate) fn new(frames: &'m HostFrames, queue: &CommandQueue, gated: bool) -> Self {
        Self {
            frames,
            ring: queue.ring(),
            status: queue.status_word(),
            head: AtomicUsize::new(0),
            handover: SpinLock::new(Handover {
                tail: 0,
                held: gated,
            }),
        }
    }

    /// Run everything handed over, and everything handed over from now on.
    pub(crate) fn release(&self) {
        let mut handover = self.handover.lock();
        handover.held = false;
        self.run(handover.tail);
    }

    /// Run the commands up to `tail`, under the handover lock.
    fn run(&self, tail: usize) {
        let mut head = self.head.load(Ordering::Relaxed);
        while head != tail {
            let word = self.frames.word(self.ring + 16 * head as u64).unwrap_or(0);
            if word >> 60 == 0xC {
                self.frames.store_word(self.status, word & 0xFFFF_FFFF);
            }
            head = (head + 1) % CommandQueue::SLOTS;
        }
        self.head.store(head, Ordering::Release);
    }
}

impl QueueRegisters for Gated<'_> {
    fn head(&self) -> Result<usize, IommuError> {
        Ok(self.head.load(Ordering::Acquire))
    }

    fn set_tail(&self, tail: usize) -> Result<(), IommuError> {
        let mut handover = self.handover.lock();
        handover.tail = tail;
        if !handover.held {
            self.run(tail);
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
}
