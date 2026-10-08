//! The command queue the queued families drive: two-word commands in a ring
//! the unit consumes from a head it advances to the tail software writes,
//! each batch closed by a completion command — one the unit stores a token
//! for, or one it consumes only once every command before it is done.
//!
//! A batch is handed over under the ring's own lock and waited on holding
//! none, each on its own completion, so batches for different domains overlap
//! their waits. A command the unit rejects is charged to the batch holding it.

use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};

use tairix_sync::SpinLock;

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

    /// The slot the unit stopped at, where it stopped on a command it
    /// rejected; it stays stopped until [`Self::resume`].
    ///
    /// # Errors
    ///
    /// The unit's refusal, or [`IommuError::Hardware`] for a unit that can run
    /// no further command.
    fn stopped_at(&self) -> Result<Option<usize>, IommuError>;

    /// Put the unit, stopped at `slot`, back to running past it — through
    /// `queue`'s [`CommandQueue::replace`].
    ///
    /// # Errors
    ///
    /// The unit's refusal.
    fn resume(
        &self,
        queue: &CommandQueue,
        memory: &TableMemory<'_>,
        slot: usize,
    ) -> Result<(), IommuError>;

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
    state: SpinLock<Ring>,
    /// Rejections charged so far, so a waiter looks for its own only when one
    /// was.
    rejections: AtomicU32,
    /// The position the unit's head was last seen at.
    consumed: AtomicU64,
}

struct Ring {
    tail: usize,
    /// The head as last read: the unit only advances it, so this never
    /// overstates the room left.
    head: usize,
    /// Commands queued so far: the position the next one takes.
    queued: u64,
    sequence: u32,
    /// The token of the batch each slot's command belongs to.
    owners: [u32; SLOTS],
    /// Tokens of batches a rejected command was charged to, until their
    /// waiters answer.
    rejected: Vec<u32>,
    /// A rejection could not be recorded, so no batch can be vouched for.
    poisoned: bool,
}

impl Ring {
    /// Take `head`, read from the unit, as its head, answering its position.
    fn observe(&mut self, head: usize) -> u64 {
        let behind = (self.tail + SLOTS - head) % SLOTS;
        self.head = head;
        self.queued - behind as u64
    }
}

/// A batch handed to its unit, to be waited on.
#[must_use]
#[derive(Debug)]
pub struct Ticket {
    token: u32,
    /// The position of its completion.
    completion: u64,
    confirm: Completion,
    /// [`CommandQueue::rejections`] as it was when the batch was handed over.
    rejections: u32,
}

/// Commands one table frame holds.
const SLOTS: usize = crate::TABLE_BYTES / core::mem::size_of::<Command>();

/// Polls of its completion a waiter makes between looks at the unit itself,
/// so many waiters do not each read its registers on every one.
const LOOK_EVERY: u32 = 16;

/// Looks a waiter tries the ring for before it takes it outright, so a busy
/// ring cannot keep it from ever looking.
const PATIENCE: u32 = 64;

/// Pages a family invalidates one command apiece before one domain-wide
/// command is the cheaper confirmation: half a ring, so a range's sync never
/// waits on the unit to make room.
pub const PAGE_INVALIDATIONS: u64 = (SLOTS / 2) as u64;

/// Whether the unit's stored `status` covers the batch of `token`: tokens
/// rise, the unit completes batches in order, and so many batches are never
/// outstanding at once that the comparison wraps.
const fn stored_covers(status: u32, token: u32) -> bool {
    status.wrapping_sub(token) < 1 << 31
}

impl CommandQueue {
    /// Commands the ring holds.
    pub const SLOTS: usize = SLOTS;

    /// An empty ring and its status word, from `memory`.
    ///
    /// # Errors
    ///
    /// [`IommuError::Exhausted`] when either, or room to charge every slot's
    /// batch a rejection, cannot be had.
    pub fn new(memory: &TableMemory<'_>) -> Result<Self, IommuError> {
        let mut rejected = Vec::new();
        rejected
            .try_reserve_exact(SLOTS)
            .map_err(|_| IommuError::Exhausted)?;
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
            state: SpinLock::new(Ring {
                tail: 0,
                head: 0,
                queued: 0,
                sequence: 0,
                owners: [0; SLOTS],
                rejected,
                poisoned: false,
            }),
            rejections: AtomicU32::new(0),
            consumed: AtomicU64::new(0),
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

    /// The physical address of the word a completion stores its token to.
    #[must_use]
    pub const fn status_word(&self) -> u64 {
        self.status.phys()
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
    /// token and the status word's address, and hand the unit the batch. A
    /// stream longer than the ring is handed over as room frees.
    ///
    /// # Errors
    ///
    /// [`IommuError::Hardware`] when the unit cannot run on past a command
    /// it rejected, [`IommuError::Unconfirmed`] when no room frees within
    /// [`COMMAND_BUDGET_NS`], or the registers' refusal.
    pub fn submit(
        &self,
        memory: &TableMemory<'_>,
        clock: &dyn Clock,
        regs: &dyn QueueRegisters,
        commands: impl IntoIterator<Item = Command>,
        completion: impl Fn(u32, u64) -> Command,
    ) -> Result<Ticket, IommuError> {
        let mut ring = self.state.lock();
        ring.sequence = ring.sequence.wrapping_add(1).max(1);
        let token = ring.sequence;
        let rejections = self.rejections.load(Ordering::Acquire);
        let wait = completion(token, self.status.phys());
        for command in commands.into_iter().chain(core::iter::once(wait)) {
            let next = (ring.tail + 1) % SLOTS;
            // The ring is full while the tail would catch the head: the unit
            // is handed what is queued so it can make room.
            if next == ring.head {
                let mut head = regs.head()? % SLOTS;
                if next == head {
                    tairix_dma_barrier::dma_wmb();
                    regs.set_tail(ring.tail)?;
                    wait_for(clock, || {
                        self.recover(&mut ring, memory, regs)?;
                        head = regs.head()? % SLOTS;
                        Ok(next != head)
                    })?;
                }
                let at = ring.observe(head);
                self.consumed.fetch_max(at, Ordering::AcqRel);
            }
            let slot = ring.tail;
            self.replace(memory, slot, command)?;
            ring.owners[slot] = token;
            ring.tail = next;
            ring.queued += 1;
        }
        tairix_dma_barrier::dma_wmb();
        regs.set_tail(ring.tail)?;
        Ok(Ticket {
            token,
            completion: ring.queued - 1,
            confirm: regs.completion(),
            rejections,
        })
    }

    /// Return once the unit has done every command of `ticket`'s batch,
    /// holding no lock while it waits.
    ///
    /// # Errors
    ///
    /// [`IommuError::Hardware`] when the unit rejected a command of the
    /// batch, or can run no further command; [`IommuError::Unconfirmed`] past
    /// [`COMMAND_BUDGET_NS`]; or the registers' refusal.
    // Taken by value so a batch's answer, its charge included, is collected
    // once.
    #[allow(clippy::needless_pass_by_value)]
    pub fn wait(
        &self,
        memory: &TableMemory<'_>,
        clock: &dyn Clock,
        regs: &dyn QueueRegisters,
        ticket: Ticket,
    ) -> Result<(), IommuError> {
        let mut polls: u32 = 0;
        let waited = wait_for(clock, || {
            if self.done(memory, &ticket)? {
                return Ok(true);
            }
            polls = polls.wrapping_add(1);
            if !polls.is_multiple_of(LOOK_EVERY) {
                return Ok(false);
            }
            // A unit that stopped never reaches the completion, and one that
            // confirms by consumption says how far it got only through its
            // head: a waiter looks now and then, where it finds the ring free,
            // and takes it once it has missed it long enough.
            let ring = if polls.is_multiple_of(LOOK_EVERY * PATIENCE) {
                Some(self.state.lock())
            } else {
                self.state.try_lock()
            };
            if let Some(mut ring) = ring {
                self.recover(&mut ring, memory, regs)?;
                if ticket.confirm == Completion::Consumed {
                    let at = ring.observe(regs.head()? % SLOTS);
                    self.consumed.fetch_max(at, Ordering::AcqRel);
                }
            }
            self.done(memory, &ticket)
        });
        // The completion was read before any charge to its batch can be: a
        // charge lands before the unit runs on to complete the batch.
        fence(Ordering::Acquire);
        let charged = self.rejections.load(Ordering::Acquire) != ticket.rejections;
        if !charged && waited.is_ok() {
            return Ok(());
        }
        let mut ring = self.state.lock();
        let rejected = ring
            .rejected
            .iter()
            .position(|&token| token == ticket.token);
        if let Some(at) = rejected {
            ring.rejected.swap_remove(at);
        }
        if rejected.is_some() || ring.poisoned {
            return Err(IommuError::Hardware);
        }
        waited
    }

    /// Hand the unit `commands` and return once it has done them all: a
    /// [`Self::submit`] waited on at once.
    ///
    /// # Errors
    ///
    /// As [`Self::submit`] and [`Self::wait`].
    pub fn run(
        &self,
        memory: &TableMemory<'_>,
        clock: &dyn Clock,
        regs: &dyn QueueRegisters,
        commands: impl IntoIterator<Item = Command>,
        completion: impl Fn(u32, u64) -> Command,
    ) -> Result<(), IommuError> {
        let ticket = self.submit(memory, clock, regs, commands, completion)?;
        self.wait(memory, clock, regs, ticket)
    }

    fn done(&self, memory: &TableMemory<'_>, ticket: &Ticket) -> Result<bool, IommuError> {
        Ok(match ticket.confirm {
            Completion::Stored => {
                let status = memory.read(&self.status, 0)?.to_le_bytes();
                let status = u32::from_le_bytes([status[0], status[1], status[2], status[3]]);
                stored_covers(status, ticket.token)
            }
            Completion::Consumed => self.consumed.load(Ordering::Acquire) > ticket.completion,
        })
    }

    /// Where the unit stopped on a command it rejected, charge the batch
    /// holding it, then put the unit back to running: charged first, so the
    /// batch's waiter cannot see it complete uncharged.
    fn recover(
        &self,
        ring: &mut Ring,
        memory: &TableMemory<'_>,
        regs: &dyn QueueRegisters,
    ) -> Result<(), IommuError> {
        let Some(slot) = regs.stopped_at()? else {
            return Ok(());
        };
        let token = ring.owners[slot % SLOTS];
        if ring.rejected.try_reserve(1).is_ok() {
            ring.rejected.push(token);
        } else {
            ring.poisoned = true;
        }
        self.rejections.fetch_add(1, Ordering::AcqRel);
        tairix_dma_barrier::dma_wmb();
        regs.resume(self, memory, slot)
    }
}

/// A unit's command queue as its family drives it: the ring, the memory it
/// lives in, the clock its waits are bounded by, its registers, and the
/// completion command its batches close with.
#[derive(Clone, Copy)]
pub struct Invalidator<'a> {
    /// The ring.
    pub queue: &'a CommandQueue,
    /// The memory the ring and its status word live in.
    pub memory: TableMemory<'a>,
    /// What every wait is bounded on.
    pub clock: &'a dyn Clock,
    /// The unit's queue registers.
    pub regs: &'a dyn QueueRegisters,
    /// The completion command closing a batch, from its token and the status
    /// word's address.
    pub completion: fn(u32, u64) -> Command,
}

impl Invalidator<'_> {
    /// [`CommandQueue::submit`] `commands`.
    ///
    /// # Errors
    ///
    /// As [`CommandQueue::submit`].
    pub fn submit(
        &self,
        commands: impl IntoIterator<Item = Command>,
    ) -> Result<Ticket, IommuError> {
        self.queue.submit(
            &self.memory,
            self.clock,
            self.regs,
            commands,
            self.completion,
        )
    }

    /// [`CommandQueue::wait`] on `ticket`.
    ///
    /// # Errors
    ///
    /// As [`CommandQueue::wait`].
    pub fn wait(&self, ticket: Ticket) -> Result<(), IommuError> {
        self.queue.wait(&self.memory, self.clock, self.regs, ticket)
    }

    /// [`CommandQueue::run`] `commands`.
    ///
    /// # Errors
    ///
    /// As [`CommandQueue::run`].
    pub fn run(&self, commands: impl IntoIterator<Item = Command>) -> Result<(), IommuError> {
        let ticket = self.submit(commands)?;
        self.wait(ticket)
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
        // Read before polling, so only a poll begun past the deadline can
        // fail the wait, however long the CPU was taken away between them.
        let expired = clock.now_ns() > deadline;
        if done()? {
            return Ok(());
        }
        if expired {
            return Err(IommuError::Unconfirmed);
        }
        core::hint::spin_loop();
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
