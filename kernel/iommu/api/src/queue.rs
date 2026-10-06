//! The command queue the queued families drive: two-word commands in a ring
//! the unit consumes from a head it advances to the tail software writes,
//! each batch confirmed by a completion command — one the unit stores a token
//! for, or one it consumes only once every command before it is done.

use crate::memory::{Table, TableMemory};
use crate::{Clock, IommuError};

/// The longest a family waits on its unit: a command batch, a register
/// handshake. Generous, because a unit that answers late is not stuck, and
/// fail closed past it.
pub const COMMAND_BUDGET_NS: u64 = 1_000_000_000;

/// One command: two 64-bit words.
pub type Command = [u64; 2];

/// What a family's queue registers say of its ring.
pub trait QueueRegisters {
    /// The slot the unit reads next.
    ///
    /// # Errors
    ///
    /// The unit's refusal.
    fn head(&self) -> Result<usize, IommuError>;

    /// Hand the unit every command before slot `tail`. Between two calls the
    /// tail moves less than a lap: the queue hands over what it has queued
    /// before the ring can fill, so a family whose unit counts laps can infer
    /// one from the tail going backwards.
    ///
    /// # Errors
    ///
    /// The unit's refusal.
    fn set_tail(&self, tail: usize) -> Result<(), IommuError>;

    /// Whether the unit stopped on a command it rejected; where it did, put
    /// the queue back to running past it — through `queue`'s
    /// [`CommandQueue::replace`] — and answer `true`.
    ///
    /// # Errors
    ///
    /// The unit's refusal.
    fn stopped(&self, queue: &CommandQueue, memory: &TableMemory<'_>) -> Result<bool, IommuError>;

    /// How the unit confirms a batch.
    fn completion(&self) -> Completion {
        Completion::Stored
    }
}

/// How a unit confirms that a batch's commands are done.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Completion {
    /// It stores the completion's token to the queue's status word.
    Stored,
    /// It moves its head past the completion only once every command before
    /// it is done (an Arm `SMMUv3`'s `CMD_SYNC` signalling nothing).
    Consumed,
}

/// A ring of commands in one table frame, and the word a completion stores
/// its token to.
pub struct CommandQueue {
    ring: Table,
    status: Table,
    tail: usize,
    /// The head as last read: the unit only advances it, so this never
    /// overstates the room left.
    head: usize,
    sequence: u32,
}

/// Commands one table frame holds.
const SLOTS: usize = crate::TABLE_BYTES / core::mem::size_of::<Command>();

impl CommandQueue {
    /// Commands the ring holds.
    pub const SLOTS: usize = SLOTS;

    /// An empty ring and its status word, from `memory`.
    ///
    /// # Errors
    ///
    /// [`IommuError::Exhausted`] when either cannot be had.
    pub fn new(memory: &TableMemory<'_>) -> Result<Self, IommuError> {
        let ring = memory.alloc()?;
        let status = match memory.alloc() {
            Ok(status) => status,
            Err(err) => {
                memory.free(ring);
                return Err(err);
            }
        };
        Ok(Self {
            ring,
            status,
            tail: 0,
            head: 0,
            sequence: 0,
        })
    }

    /// Give the ring and status frames back: for a queue no unit was ever
    /// pointed at.
    pub fn release(self, memory: &TableMemory<'_>) {
        memory.free(self.ring);
        memory.free(self.status);
    }

    /// The ring's physical address.
    #[must_use]
    pub const fn ring(&self) -> u64 {
        self.ring.phys()
    }

    /// Write `command` over slot `slot` where the unit will read it: how a
    /// family puts a rejected command out of the unit's way.
    ///
    /// # Errors
    ///
    /// The table write's refusal.
    pub fn replace(
        &self,
        memory: &TableMemory<'_>,
        slot: usize,
        command: Command,
    ) -> Result<(), IommuError> {
        let slot = slot % SLOTS;
        memory.write(&self.ring, 2 * slot, command[0])?;
        memory.write(&self.ring, 2 * slot + 1, command[1])?;
        memory.publish(&self.ring, 2 * slot, 2);
        Ok(())
    }

    /// Queue `commands`, then the completion `completion` makes of a fresh
    /// token and the status word's address, and return once the unit has
    /// stored the token: every command before it is done. A stream longer
    /// than the ring is handed over as room frees.
    ///
    /// # Errors
    ///
    /// [`IommuError::Hardware`] when the unit rejected a command,
    /// [`IommuError::Unconfirmed`] past [`COMMAND_BUDGET_NS`], or the
    /// registers' refusal.
    pub fn run(
        &mut self,
        memory: &TableMemory<'_>,
        clock: &dyn Clock,
        regs: &dyn QueueRegisters,
        commands: impl IntoIterator<Item = Command>,
        completion: impl Fn(u32, u64) -> Command,
    ) -> Result<(), IommuError> {
        self.sequence = self.sequence.wrapping_add(1).max(1);
        let token = self.sequence;
        let wait = completion(token, self.status.phys());
        for command in commands.into_iter().chain(core::iter::once(wait)) {
            let next = (self.tail + 1) % SLOTS;
            // The ring is full while the tail would catch the head: the unit
            // is handed what is queued so it can make room.
            if next == self.head {
                let mut head = regs.head()? % SLOTS;
                if next == head {
                    tairix_dma_barrier::dma_wmb();
                    regs.set_tail(self.tail)?;
                    let queue = &*self;
                    wait_for(clock, || {
                        head = regs.head()? % SLOTS;
                        if next != head {
                            return Ok(true);
                        }
                        if regs.stopped(queue, memory)? {
                            return Err(IommuError::Hardware);
                        }
                        Ok(false)
                    })?;
                }
                self.head = head;
            }
            self.replace(memory, self.tail, command)?;
            self.tail = next;
        }
        tairix_dma_barrier::dma_wmb();
        regs.set_tail(self.tail)?;
        let completion = regs.completion();
        wait_for(clock, || {
            let done = match completion {
                Completion::Stored => {
                    memory.read(&self.status, 0)?.to_le_bytes()[..4] == token.to_le_bytes()
                }
                Completion::Consumed => regs.head()? % SLOTS == self.tail,
            };
            if done {
                return Ok(true);
            }
            if regs.stopped(self, memory)? {
                return Err(IommuError::Hardware);
            }
            Ok(false)
        })?;
        if completion == Completion::Consumed {
            self.head = self.tail;
        }
        Ok(())
    }
}

/// Spin until `done` answers `true`, or fail [`IommuError::Unconfirmed`] once
/// [`COMMAND_BUDGET_NS`] has passed on `clock`: every wait a family makes on
/// its unit, which answers within a bounded handshake or is broken.
///
/// # Errors
///
/// [`IommuError::Unconfirmed`] past the budget, or `done`'s own error.
pub fn wait_for(
    clock: &dyn Clock,
    done: impl FnMut() -> Result<bool, IommuError>,
) -> Result<(), IommuError> {
    wait_within(clock, COMMAND_BUDGET_NS, done)
}

/// [`wait_for`] with a budget of `budget_ns` of its own, for a wait shorter
/// than a handshake's.
///
/// # Errors
///
/// [`IommuError::Unconfirmed`] past the budget, or `done`'s own error.
pub fn wait_within(
    clock: &dyn Clock,
    budget_ns: u64,
    mut done: impl FnMut() -> Result<bool, IommuError>,
) -> Result<(), IommuError> {
    let deadline = clock.now_ns().saturating_add(budget_ns);
    loop {
        if done()? {
            return Ok(());
        }
        if clock.now_ns() > deadline {
            return Err(IommuError::Unconfirmed);
        }
        core::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;
    use core::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::hostmem::HostFrames;

    /// A unit that consumes the ring up to the tail the moment it is set and
    /// stores each completion's token, or stops on a command naming `reject`.
    struct Unit<'m> {
        frames: &'m HostFrames,
        ring: u64,
        status: u64,
        head: Cell<usize>,
        head_reads: Cell<usize>,
        reject: u64,
        stopped: Cell<bool>,
    }

    impl QueueRegisters for Unit<'_> {
        fn head(&self) -> Result<usize, IommuError> {
            self.head_reads.set(self.head_reads.get() + 1);
            Ok(self.head.get())
        }

        fn set_tail(&self, tail: usize) -> Result<(), IommuError> {
            while self.head.get() != tail {
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

        fn stopped(
            &self,
            queue: &CommandQueue,
            memory: &TableMemory<'_>,
        ) -> Result<bool, IommuError> {
            if !self.stopped.replace(false) {
                return Ok(false);
            }
            queue.replace(memory, self.head.get(), [0, 0])?;
            Ok(true)
        }
    }

    /// A completion as the test unit reads it: its token in the low word.
    fn completion(token: u32, _status: u64) -> Command {
        [0xC << 60 | u64::from(token), 0]
    }

    struct Clock0(AtomicU64);

    impl Clock for Clock0 {
        fn now_ns(&self) -> u64 {
            self.0.fetch_add(1_000, Ordering::Relaxed)
        }
    }

    fn rig<'m>(frames: &'m HostFrames, queue: &CommandQueue, reject: u64) -> Unit<'m> {
        Unit {
            frames,
            ring: queue.ring(),
            status: queue.status.phys(),
            head: Cell::new(0),
            head_reads: Cell::new(0),
            reject,
            stopped: Cell::new(false),
        }
    }

    #[test]
    fn a_batch_returns_once_its_completion_is_stored_and_wraps_the_ring() {
        let frames = HostFrames::new(0x1_0000_0000);
        let memory = TableMemory::new(&frames, None);
        let mut queue = CommandQueue::new(&memory).unwrap();
        let unit = rig(&frames, &queue, u64::MAX);
        let clock = Clock0(AtomicU64::new(0));
        for round in 0..(2 * SLOTS) {
            queue
                .run(&memory, &clock, &unit, [[round as u64, 0]], completion)
                .unwrap();
        }
        assert_eq!(unit.head.get(), queue.tail, "the unit consumed every slot");
    }

    /// A stream longer than the ring is handed over as the unit makes room,
    /// with the head register read only when the ring looks full.
    #[test]
    fn a_stream_longer_than_the_ring_is_handed_over_as_room_frees() {
        let frames = HostFrames::new(0x1_0000_0000);
        let memory = TableMemory::new(&frames, None);
        let mut queue = CommandQueue::new(&memory).unwrap();
        let unit = rig(&frames, &queue, u64::MAX);
        let clock = Clock0(AtomicU64::new(0));
        let stream = (0..3 * SLOTS as u64).map(|command| [command, 0]);
        queue
            .run(&memory, &clock, &unit, stream, completion)
            .unwrap();
        assert_eq!(
            unit.head.get(),
            queue.tail,
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
    fn a_rejected_command_is_reported_and_the_queue_runs_on() {
        let frames = HostFrames::new(0x1_0000_0000);
        let memory = TableMemory::new(&frames, None);
        let mut queue = CommandQueue::new(&memory).unwrap();
        let unit = rig(&frames, &queue, 0xBAD);
        let clock = Clock0(AtomicU64::new(0));
        assert_eq!(
            queue.run(&memory, &clock, &unit, [[0xBAD, 0]], completion),
            Err(IommuError::Hardware)
        );
        unit.set_tail(queue.tail).unwrap();
        queue
            .run(&memory, &clock, &unit, [[1, 0]], completion)
            .unwrap();
    }

    /// A unit confirming by consumption, which stores nothing; it stops
    /// consuming before slot `stall` where one is given.
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

        fn stopped(
            &self,
            _queue: &CommandQueue,
            _memory: &TableMemory<'_>,
        ) -> Result<bool, IommuError> {
            Ok(false)
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
        let mut queue = CommandQueue::new(&memory).unwrap();
        let clock = Clock0(AtomicU64::new(0));
        let unit = Consuming {
            head: Cell::new(0),
            stall: None,
        };
        for round in 0..(2 * SLOTS) {
            queue
                .run(&memory, &clock, &unit, [[round as u64, 0]], completion)
                .unwrap();
        }
        assert_eq!(unit.head.get(), queue.tail);
        let stuck = Consuming {
            head: Cell::new(queue.tail),
            stall: Some((queue.tail + 1) % SLOTS),
        };
        assert_eq!(
            queue.run(&memory, &clock, &stuck, [[1, 0]], completion),
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
            fn stopped(
                &self,
                _queue: &CommandQueue,
                _memory: &TableMemory<'_>,
            ) -> Result<bool, IommuError> {
                Ok(false)
            }
        }
        let frames = HostFrames::new(0x1_0000_0000);
        let memory = TableMemory::new(&frames, None);
        let mut queue = CommandQueue::new(&memory).unwrap();
        let clock = Clock0(AtomicU64::new(0));
        assert_eq!(
            queue.run(&memory, &clock, &Silent, [[1, 0]], completion),
            Err(IommuError::Unconfirmed)
        );
    }
}
