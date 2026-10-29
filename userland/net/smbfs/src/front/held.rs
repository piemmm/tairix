//! The files the kernel holds open on a mounted share.
//!
//! Each is kept open on the server under a lease. While the lease lets it,
//! what is read is cached and what is written is held, until the write-back
//! window, the bound on unwritten bytes, a sync, a close or the lease's break
//! asks for it. One file's work runs in the order it was asked, so no write
//! overtakes a truncate, and no read the write before it.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::NodeId;
use tairix_abi::fsserve::{Answer, Span};
use tairix_abi::netattach::OfferId;
use tairix_abi::netshare_ipc::MountState;
use tairix_abi::time::NANOS_PER_SEC;
use tairix_abi::DriverError;
use tairix_collections::{HashMap, IndexedHeap};
use tairix_hash::{BuildFastHash, BuildSipHash13};
use tairix_inline::ArrayVec;
use tairix_log::{Field, FieldValue, Level, Sink};
use tairix_reclaim::{
    CacheBudget, CacheCandidate, CachedBytes, InvalidationSource, PressureBand, PressureGauge,
    RebuildCost, ReclaimCache, ReclaimClass, ReclaimOwner, ReclaimRule, Sensitivity,
    MAP_ENTRY_OVERHEAD,
};
use tairix_writeback::WritebackBound;

use super::served::{driver_error, Through};
use super::written::{Written, BLOCK, BLOCK_PINNED};
use super::{record, Front, Lie, CALL_DEADLINE_NS};
use crate::events::WRITE_BACK_LOST;
use crate::host::Host;
use crate::wire::{Durability, FileOp, Order, Report, Stat, CHUNK_MAX, IO_MAX};

/// Bytes one cached page holds: a lane's span, so one call's span covers at
/// most two pages.
pub const PAGE: usize = IO_MAX;

/// [`PAGE`], as a read asks for it.
const PAGE_LEN: u32 = 64 << 10;

const _: () = assert!(PAGE_LEN as usize == PAGE);

/// How long written bytes wait before they are sent.
pub const WRITE_BACK_NS: u64 = NANOS_PER_SEC as u64;

/// The most writes of one file's bytes in flight at once, so a file sent
/// whole never queues more than this toward its worker.
const SENDING_MAX: usize = 8;

/// The most blocks' bytes one staged chunk gathers.
const CHUNK_PARTS: usize = CHUNK_MAX.div_ceil(BLOCK) + 1;

/// The lease bit letting reads be cached.
const READ: u8 = 0b001;

/// The lease bit letting writes be held.
const WRITE: u8 = 0b100;

/// The label a page cache's audit records carry.
const PAGE_CACHE_LABEL: &str = "smbfs pages";

/// One cached page's key: the file, which of its generations, which page.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(super) struct PageKey {
    offer: u64,
    node: u64,
    generation: u32,
    page: u64,
}

/// One page's bytes, as the server last had them: short where the file
/// ended within it.
pub(super) struct Page(Vec<u8>);

impl CachedBytes for Page {
    fn payload_bytes(&self) -> usize {
        self.0.capacity()
    }

    fn wipe(&mut self) {
        self.0.fill(0);
    }
}

/// The clean pages of every held file the front serves.
pub(super) type Pages = ReclaimCache<PageKey, Page, (), BuildSipHash13>;

/// A cache of clean pages within `budget`.
pub(super) fn page_cache(
    budget: CacheBudget,
    pressure: &'static dyn PressureGauge,
    sink: &'static (dyn Sink + Sync),
    hasher: BuildSipHash13,
) -> Pages {
    let candidate = CacheCandidate {
        class: Some(ReclaimClass::CleanFileData),
        owner: Some(ReclaimOwner::UserlandProcess("smbfs")),
        rebuild_cost: RebuildCost::Moderate,
        sensitivity: Some(Sensitivity::UserData),
        // A page is keyed by its file's generation, which a lease's break
        // or a resize moves on.
        invalidation: Some(InvalidationSource::GenerationToken),
        rule: Some(ReclaimRule::Drop),
        entry_metadata_bytes: core::mem::size_of::<PageKey>() + MAP_ENTRY_OVERHEAD,
    };
    ReclaimCache::new(PAGE_CACHE_LABEL, candidate, budget, pressure, sink, hasher)
}

/// One step of a held file's work.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Step {
    /// The kernel's call `ticket` on the file.
    Call(u64),
    /// Send every byte written so far.
    Wave,
    /// Acknowledge the lease's break, keeping these lease bits.
    Ack(u8),
    /// Send every byte written so far and have the server commit them, for
    /// the volume's flush of this name.
    Sync(u32),
    /// Send every byte written so far and let the open go.
    Close,
}

/// A page a head read waits for.
pub(super) struct Fetch {
    request: u32,
    page: u64,
    bytes: Vec<u8>,
    done: bool,
}

/// How a held file is reached.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Reach {
    /// Its open not asked for yet, or let go: its next call asks for one.
    Unopened,
    /// Through the open the server holds.
    Open,
    /// By path, the server having not held an open.
    ByPath,
}

/// How a held file's open is asked for again once its connection is made
/// again.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Reopen {
    /// Its durable open, taken back.
    Reclaim,
    /// Its open asked again under the name it was first asked by, which a
    /// server answers with the open the first ask made, if it made one, and
    /// with a new one if not.
    Replay,
}

/// What a held file waits on the server for.
pub(super) enum Active {
    /// Its open, or the open asked again after its connection dropped.
    Opening(u32, Option<Reopen>),
    /// The pages the head read `ticket` needs: one or two.
    Fetching(u64, Vec<Fetch>),
    /// Its written bytes going out.
    Sending(Wave),
    /// The head call, which the server answers.
    Call(u64),
    /// A sync, for the volume's flush.
    Syncing(u32, u32),
    /// A break's acknowledgment, keeping these lease bits.
    Acking(u32, u8),
    /// Its close.
    Closing(u32),
}

/// One write of a wave in flight: its request, and the bytes it carries.
#[derive(Copy, Clone)]
struct Flight {
    offset: u64,
    request: u32,
    len: u32,
}

/// A held file's written bytes going out, at most [`SENDING_MAX`] writes at
/// a time. The bytes stay held until the server takes them.
pub(super) struct Wave {
    flight: ArrayVec<Flight, SENDING_MAX>,
    /// Where the next write's bytes are looked for from.
    cursor: u64,
    /// Whether its writes may be replays of ones a dropped connection took
    /// the answers of.
    replay: bool,
}

/// Bytes of a held file the server did not take.
#[derive(Copy, Clone)]
enum Lost {
    Stretch(u64, usize),
    All,
}

/// A call on a held file, as it was asked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum HeldOp {
    Read { offset: u64, span: Span },
    Write { offset: u64, span: Span },
    Truncate(u64),
}

/// What a held call the server answered did to its file.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Effect {
    /// Nothing the front keeps: a read, or a refusal.
    Nothing,
    /// `count` bytes written from `offset`.
    Wrote { offset: u64, count: u64 },
    /// The file's length set.
    Resized(u64),
}

/// A file the kernel holds open, and the front's open on it.
pub(super) struct Held {
    pub(super) handle: u32,
    pub(super) key: [u8; 16],
    /// Its place among the volume's deadlines.
    slot: usize,
    /// The name its durable open is asked by, the client's own.
    create_guid: [u8; 16],
    /// Its open's id on the server, and how long the open outlives a
    /// dropped connection, where the server made it durable.
    durable: Option<Durable>,
    /// How its open is asked for again, once its connection is back.
    reopen: Option<Reopen>,
    /// The step a dropped connection interrupted, which runs first once
    /// the open is back.
    interrupted: Option<Step>,
    /// Whether the next bytes it sends may be replays of some sent before
    /// the connection dropped.
    replay_writes: bool,
    /// The lease bits granted.
    lease: u8,
    reach: Reach,
    /// The file's length as the kernel sees it.
    size: u64,
    /// The server's last word on what the file is, its times moved on by
    /// what is held; none before it is open, and none once a write it was
    /// told of was lost, so nothing more is cached of it.
    stat: Option<Stat>,
    /// Keys the file's clean pages: a new one makes every cached page
    /// unreachable at once.
    generation: u32,
    written: Written,
    steps: VecDeque<Step>,
    active: Option<Active>,
    /// When what it waits on was asked, or last moved on.
    active_since: u64,
    /// No write-back is tried before this, after one could not start.
    not_before: u64,
    /// Whether it was written since it was last committed.
    unsynced: bool,
}

impl Held {
    /// Whether the server holds its open.
    pub(super) fn is_open(&self) -> bool {
        self.reach == Reach::Open
    }

    /// What the kernel is told of the file while the lease keeps others'
    /// writes out: the server's word, with what is held laid over it.
    pub(super) fn known(&self) -> Option<Stat> {
        if !self.is_open() || self.lease & READ == 0 {
            return None;
        }
        self.stat.map(|stat| Stat {
            size: self.size,
            ..stat
        })
    }

    /// When the file next needs the front unasked: what it waits on given
    /// up, or its written bytes due out.
    fn deadline(&self) -> Option<u64> {
        if self.active.is_some() {
            return Some(self.active_since.saturating_add(CALL_DEADLINE_NS));
        }
        // Work it has waiting runs as soon as the connection is back.
        if self.reopen.is_some() || self.interrupted.is_some() || !self.steps.is_empty() {
            return Some(0);
        }
        let since = self.written.since().filter(|_| self.is_open())?;
        Some(since.saturating_add(WRITE_BACK_NS).max(self.not_before))
    }
}

/// A durable open: its id, and how long it outlives a dropped connection.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Durable {
    file_id: [u8; 16],
    timeout_ms: u32,
}

/// What a volume's written bytes not yet confirmed hold.
#[derive(Copy, Clone, Default)]
struct Unsent {
    bytes: usize,
    /// Memory pinned holding them, which the bound bounds.
    pinned: usize,
    blocks: usize,
}

impl Unsent {
    fn of(written: &Written) -> Self {
        Self {
            bytes: written.bytes(),
            pinned: written.pinned(),
            blocks: written.blocks(),
        }
    }
}

/// A volume's held files, and the bound on what they hold written.
pub(super) struct Holding {
    pub(super) files: HashMap<u64, Held, BuildFastHash>,
    /// The held file each request in flight is for.
    pub(super) ops: HashMap<u32, u64, BuildFastHash>,
    /// The held file each lease key is for.
    keys: HashMap<[u8; 16], u64, BuildFastHash>,
    /// Each held file's deadline, by its slot.
    deadlines: IndexedHeap<u64>,
    /// The file each slot is given to.
    slots: Vec<u64>,
    /// Slots given back, to be given again.
    free: Vec<usize>,
    /// The bound on what written bytes not yet confirmed pin; with none,
    /// every write is sent before it is answered.
    bound: Option<WritebackBound>,
    unsent: Unsent,
    /// The first write the server did not take since the volume was last
    /// flushed, owed to the next flush.
    lost: Option<DriverError>,
    /// The volume's flushes waiting on syncs: each name and how many.
    flushes: Vec<(u32, usize)>,
    next_flush: u32,
    /// Whether its connection is down: no held work runs and nothing held
    /// is due until it is back.
    pub(super) paused: bool,
}

impl Holding {
    pub(super) fn new(bound: Option<WritebackBound>) -> Self {
        Self {
            files: HashMap::with_hasher(BuildFastHash::new()),
            ops: HashMap::with_hasher(BuildFastHash::new()),
            keys: HashMap::with_hasher(BuildFastHash::new()),
            deadlines: IndexedHeap::new(),
            slots: Vec::new(),
            free: Vec::new(),
            bound,
            unsent: Unsent::default(),
            lost: None,
            flushes: Vec::new(),
            next_flush: 0,
            paused: false,
        }
    }

    /// Whether the volume holds bytes the server does not have.
    pub(super) const fn unpublished(&self) -> bool {
        self.unsent.bytes > 0
    }

    /// Bytes the volume holds that the server does not have.
    pub(super) const fn unsent_bytes(&self) -> usize {
        self.unsent.bytes
    }

    /// The longest any held file's open outlives a dropped connection, if
    /// any is durable.
    pub(super) fn longest_durable_ms(&self) -> Option<u32> {
        self.files
            .values()
            .filter_map(|held| held.durable)
            .map(|durable| durable.timeout_ms)
            .max()
    }

    /// Let go of the flushes waiting on syncs, whose calls were answered.
    pub(super) fn forget_flushes(&mut self) {
        self.flushes.clear();
    }

    /// The connection is down: each file's work stops where it is, the step
    /// its server work was part of kept to run first, and its open is to be
    /// asked for again once the connection is back.
    fn suspend(&mut self) {
        self.paused = true;
        self.ops.clear();
        let Self {
            files, deadlines, ..
        } = self;
        for held in files.values_mut() {
            let active = held.active.take();
            let asked = match active {
                Some(Active::Opening(_, reopen)) => Some(reopen.unwrap_or(Reopen::Replay)),
                _ => None,
            };
            let step = match active {
                Some(Active::Fetching(ticket, _) | Active::Call(ticket)) => {
                    Some(Step::Call(ticket))
                }
                Some(Active::Sending(_)) => Some(Step::Wave),
                Some(Active::Syncing(_, flush)) => Some(Step::Sync(flush)),
                Some(Active::Acking(_, bits)) => Some(Step::Ack(bits)),
                Some(Active::Closing(_)) => Some(Step::Close),
                Some(Active::Opening(..)) | None => None,
            };
            if step.is_some() {
                held.interrupted = step;
            }
            let held_open = held.is_open().then_some(if held.durable.is_some() {
                Reopen::Reclaim
            } else {
                Reopen::Replay
            });
            held.reopen = asked.or(held_open).or(held.reopen);
            if held_open.is_some() {
                held.reach = Reach::Unopened;
            }
            match held.deadline() {
                // Room was made with the slot.
                Some(at) => {
                    let _ = deadlines.try_insert(held.slot, at);
                }
                None => {
                    deadlines.remove(held.slot);
                }
            }
        }
    }

    /// The connection was not back within its grace window: what every file
    /// holds written is lost, each open is gone with the server's wait, and
    /// so is every file, to be held afresh by its next call. The bytes lost.
    fn give_up_all(&mut self) -> usize {
        let lost = self.unsent.bytes;
        let Self {
            files,
            keys,
            deadlines,
            free,
            ..
        } = self;
        files.retain(|_, held| {
            keys.remove(&held.key);
            deadlines.remove(held.slot);
            free.push(held.slot);
            false
        });
        self.recount(self.unsent, Unsent::default());
        if lost > 0 {
            self.lost.get_or_insert(DriverError::DeviceOffline);
        }
        lost
    }

    /// The earliest instant a held file needs the front unasked.
    pub(super) fn next_deadline(&self) -> Option<u64> {
        if self.paused {
            return None;
        }
        self.deadlines.peek().map(|(_, at)| at)
    }

    /// A held file whose deadline has come by `now`; none while paused.
    fn due(&self, now: u64) -> Option<u64> {
        if self.paused {
            return None;
        }
        let (slot, _) = self.deadlines.peek().filter(|&(_, at)| at <= now)?;
        self.slots.get(slot).copied()
    }

    /// Keep `held` as `node`'s, giving it a slot among the deadlines.
    fn keep(&mut self, node: u64, mut held: Held) -> Result<(), DriverError> {
        let slot = self.free.last().copied().unwrap_or(self.slots.len());
        let fresh = slot == self.slots.len();
        // The free list has room for every slot ever given, so giving one
        // back never allocates.
        let room = self.files.try_reserve(1).is_ok()
            && self.keys.try_reserve(1).is_ok()
            && self.deadlines.try_reserve_slot(slot).is_ok()
            && (!fresh
                || (self.slots.try_reserve(1).is_ok()
                    && self.free.try_reserve(slot + 1 - self.free.len()).is_ok()));
        if !room {
            return Err(DriverError::OutOfMemory);
        }
        if fresh {
            self.slots.push(node);
        } else {
            self.free.pop();
            self.slots[slot] = node;
        }
        held.slot = slot;
        // Room was made above.
        let _ = self.keys.try_insert(held.key, node);
        let _ = self.files.try_insert(node, held);
        Ok(())
    }

    /// Let `node` go, and its slot.
    fn forget(&mut self, node: u64) {
        let Some(held) = self.files.remove(&node) else {
            return;
        };
        self.keys.remove(&held.key);
        self.deadlines.remove(held.slot);
        self.free.push(held.slot);
        self.recount(Unsent::of(&held.written), Unsent::default());
    }

    /// Bring `node`'s deadline up to date.
    fn retime(&mut self, node: u64) {
        let Some(held) = self.files.get(&node) else {
            return;
        };
        match held.deadline() {
            // Room was made with the slot.
            Some(at) => {
                let _ = self.deadlines.try_insert(held.slot, at);
            }
            None => {
                self.deadlines.remove(held.slot);
            }
        }
    }

    /// One file's written bytes went from `before` to `after`.
    fn recount(&mut self, before: Unsent, after: Unsent) {
        let total = &mut self.unsent;
        total.bytes = total
            .bytes
            .saturating_add(after.bytes)
            .saturating_sub(before.bytes);
        total.pinned = total
            .pinned
            .saturating_add(after.pinned)
            .saturating_sub(before.pinned);
        total.blocks = total
            .blocks
            .saturating_add(after.blocks)
            .saturating_sub(before.blocks);
        if let Some(bound) = &self.bound {
            bound.publish(total.pinned, total.blocks as u64);
        }
    }

    /// Whether `more` bytes more may be pinned holding writes: none under
    /// critical pressure, where nothing speculative runs, and otherwise
    /// always one write's worth when nothing is held, so holding goes on at
    /// any ceiling.
    fn admits(&self, more: usize) -> bool {
        self.bound.as_ref().is_some_and(|bound| {
            let reading = bound.reading(self.unsent.pinned);
            reading.band != PressureBand::Critical
                && (self.unsent.blocks == 0
                    || self.unsent.pinned.saturating_add(more) <= reading.ceiling)
        })
    }

    /// The bound held a write back.
    fn note_refusal(&self) {
        if let Some(bound) = &self.bound {
            bound.note_refusal();
        }
    }

    /// The bound sent what a file held, to make room.
    fn note_released(&self) {
        if let Some(bound) = &self.bound {
            bound.note_released();
        }
    }
}

/// How many of a read's `span` bytes from `offset` a file of `size` holds.
fn within(offset: u64, span: Span, size: u64) -> usize {
    usize::try_from(size.saturating_sub(offset)).map_or(span.size(), |left| left.min(span.size()))
}

/// The pages of a file's bytes `[from, to)`, `to` past `from`.
fn pages(from: u64, to: u64) -> core::ops::RangeInclusive<u64> {
    from / PAGE as u64..=(to - 1) / PAGE as u64
}

/// Lay `bytes`, the file's page `page`, over `room`, which holds the file
/// from `offset`.
fn lay(room: &mut [u8], offset: u64, page: u64, bytes: &[u8]) {
    let start = page * PAGE as u64;
    let from = start.max(offset);
    let to = (start + bytes.len() as u64).min(offset + room.len() as u64);
    if from >= to {
        return;
    }
    // Within one page and one window, so each fits.
    let (Ok(src), Ok(dst), Ok(len)) = (
        usize::try_from(from - start),
        usize::try_from(from - offset),
        usize::try_from(to - from),
    ) else {
        return;
    };
    room[dst..dst + len].copy_from_slice(&bytes[src..src + len]);
}

/// Whether a step ran to its end, waits on the server, or must run again
/// once what it waits behind is done.
enum Ran {
    Done,
    Waiting,
    Again,
}

impl<H: Host, S: Sink + Clone> Front<H, S> {
    /// A fresh name for one open the front keeps.
    pub(super) fn next_handle(&mut self) -> u32 {
        self.next_open = self.next_open.wrapping_add(1);
        self.next_open
    }

    fn held_mut(&mut self, offer: OfferId, node: NodeId) -> Option<&mut Held> {
        self.volumes
            .get_mut(&offer.get())?
            .held
            .files
            .get_mut(&node.raw())
    }

    fn held(&self, offer: OfferId, node: NodeId) -> Option<&Held> {
        self.volumes.get(&offer.get())?.held.files.get(&node.raw())
    }

    /// Take the call `ticket` on `node`, a file the kernel holds open, or
    /// `None` for one reached by path.
    pub(super) fn held_call(
        &mut self,
        now: u64,
        offer: OfferId,
        ticket: u64,
        node: NodeId,
        op: HeldOp,
    ) -> Option<Result<(), DriverError>> {
        let volume = self.volumes.get_mut(&offer.get())?;
        match volume.held.files.get(&node.raw()) {
            Some(held) if held.reach == Reach::ByPath => return None,
            None if volume.retiring.is_some() => return None,
            // An open the front holds serves the file even once its name is
            // gone, as an open file outlives its name.
            Some(_) => {}
            None => {
                let found = volume.nodes.get(node)?;
                if found.directory || !volume.nodes.is_pinned(node) {
                    return None;
                }
            }
        }
        if volume.calls.try_reserve(1).is_err() {
            return Some(Err(DriverError::OutOfMemory));
        }
        if self.reach(offer) == MountState::Lost {
            return Some(Err(DriverError::DeviceOffline));
        }
        let volume = self.volumes.get_mut(&offer.get())?;
        if !volume.held.files.contains_key(&node.raw()) {
            let (mut key, mut create_guid) = ([0; 16], [0; 16]);
            if self.host.fill_random(&mut key).is_err()
                || self.host.fill_random(&mut create_guid).is_err()
            {
                return Some(Err(DriverError::DeviceFault));
            }
            let handle = self.next_handle();
            let held = Held {
                handle,
                key,
                slot: 0,
                create_guid,
                durable: None,
                reopen: None,
                interrupted: None,
                replay_writes: false,
                lease: 0,
                reach: Reach::Unopened,
                size: 0,
                stat: None,
                generation: 0,
                written: Written::default(),
                steps: VecDeque::new(),
                active: None,
                active_since: now,
                not_before: 0,
                unsynced: false,
            };
            let volume = self.volumes.get_mut(&offer.get())?;
            if let Err(error) = volume.held.keep(node.raw(), held) {
                return Some(Err(error));
            }
        }
        let volume = self.volumes.get_mut(&offer.get())?;
        let held = volume.held.files.get_mut(&node.raw())?;
        if held.steps.try_reserve(1).is_err() {
            return Some(Err(DriverError::OutOfMemory));
        }
        held.steps.push_back(Step::Call(ticket));
        volume.hold_call(now, ticket, node, op);
        self.pump_held(now, offer, node);
        Some(Ok(()))
    }

    /// Run `node`'s steps until one waits on the server, and bring its
    /// deadline up to date.
    pub(super) fn pump_held(&mut self, now: u64, offer: OfferId, node: NodeId) {
        self.run_held(now, offer, node);
        if let Some(volume) = self.volumes.get_mut(&offer.get()) {
            volume.held.retime(node.raw());
        }
    }

    fn run_held(&mut self, now: u64, offer: OfferId, node: NodeId) {
        loop {
            let Some(volume) = self.volumes.get_mut(&offer.get()) else {
                return;
            };
            if volume.held.paused {
                return;
            }
            let Some(held) = volume.held.files.get_mut(&node.raw()) else {
                return;
            };
            if held.active.is_some() {
                return;
            }
            if let Some(reopen) = held.reopen {
                self.reopen_held(offer, node, reopen);
                if let Some(held) = self.held_mut(offer, node) {
                    held.active_since = now;
                }
                continue;
            }
            let interrupted = held.interrupted.take();
            let Some(step) = interrupted.or_else(|| held.steps.pop_front()) else {
                if held.reach == Reach::Unopened {
                    volume.held.forget(node.raw());
                    if volume.held.files.is_empty() && volume.retiring.is_some() {
                        self.retired(now, offer);
                    }
                }
                return;
            };
            match self.run_step(now, offer, node, step) {
                Ran::Done => {}
                Ran::Waiting => {
                    if let Some(held) = self.held_mut(offer, node) {
                        held.active_since = now;
                    }
                    return;
                }
                Ran::Again => {
                    if let Some(held) = self.held_mut(offer, node) {
                        // Back where it came from, which has room for it.
                        if interrupted.is_some() {
                            held.interrupted = Some(step);
                        } else {
                            held.steps.push_front(step);
                        }
                        held.active_since = now;
                    }
                }
            }
        }
    }

    /// Run one step of `node`'s work.
    fn run_step(&mut self, now: u64, offer: OfferId, node: NodeId, step: Step) -> Ran {
        let Some(volume) = self.volumes.get(&offer.get()) else {
            return Ran::Done;
        };
        let retiring = volume.retiring.is_some();
        let Some(held) = volume.held.files.get(&node.raw()) else {
            return Ran::Done;
        };
        let (reach, unsent) = (held.reach, !held.written.is_empty());
        match step {
            Step::Call(ticket) => match self.held_kind(offer, ticket) {
                // A call answered meanwhile, by its deadline.
                None => Ran::Done,
                // A volume going opens nothing more: the call goes by path.
                Some(true) if reach == Reach::Unopened && !retiring => self.open_held(offer, node),
                Some(true) => self.run_call(now, offer, node, ticket),
                Some(false) => match self.order_prepared(now, offer, ticket) {
                    Ok(()) => {
                        self.set_active(offer, node, Active::Call(ticket));
                        Ran::Waiting
                    }
                    Err(error) => {
                        self.answer_held(offer, ticket, Err(error));
                        Ran::Done
                    }
                },
            },
            Step::Wave | Step::Sync(_) | Step::Close | Step::Ack(_)
                if unsent && reach == Reach::Open =>
            {
                self.send_wave(offer, node);
                Ran::Again
            }
            Step::Wave => Ran::Done,
            Step::Sync(flush) => self.sync_held(offer, node, flush),
            Step::Ack(bits) => self.ack_held(offer, node, bits),
            Step::Close if reach == Reach::Open => self.close_held(offer, node),
            Step::Close => {
                // Reached by path while it was held; nothing is open to let go.
                if let Some(held) = self.held_mut(offer, node) {
                    held.reach = Reach::Unopened;
                }
                Ran::Done
            }
        }
    }

    /// Order `op` for `node` as `request`; whether it went.
    fn held_order(&mut self, offer: OfferId, node: NodeId, request: u32, op: &FileOp<'_>) -> bool {
        let Self {
            volumes,
            conns,
            routes,
            ..
        } = self;
        let Some(volume) = volumes.get_mut(&offer.get()) else {
            return false;
        };
        if routes.try_reserve(1).is_err() || volume.held.ops.try_reserve(1).is_err() {
            return false;
        }
        let tree = volume.tree;
        let ordered = conns
            .get_mut(volume.conn)
            .and_then(|conn| conn.link.as_mut())
            .is_some_and(|link| {
                link.order(&Order::File {
                    request,
                    tree,
                    op: *op,
                })
                .is_ok()
            });
        if ordered {
            // Room was reserved above.
            let _ = routes.try_insert(request, offer.get());
            let _ = volume.held.ops.try_insert(request, node.raw());
        }
        ordered
    }

    fn set_active(&mut self, offer: OfferId, node: NodeId, active: Active) {
        if let Some(held) = self.held_mut(offer, node) {
            held.active = Some(active);
        }
    }

    /// Order `node`'s open, under its lease key; one that cannot go leaves
    /// the file reached by path.
    fn open_held(&mut self, offer: OfferId, node: NodeId) -> Ran {
        let request = self.next_request_id();
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return Ran::Done;
        };
        let Some(path) = volume.nodes.get(node).map(|found| found.path) else {
            // A node that lost its name before it was ever opened is
            // reached by nothing.
            if let Some(held) = volume.held.files.get_mut(&node.raw()) {
                held.reach = Reach::ByPath;
            }
            return Ran::Again;
        };
        let Some(held) = volume.held.files.get(&node.raw()) else {
            return Ran::Done;
        };
        let (handle, key, write) = (held.handle, held.key, !volume.read_only);
        let op = FileOp::Hold {
            path: path.as_str(),
            handle,
            key,
            directory: false,
            write,
            durability: Durability::Ask {
                create_guid: held.create_guid,
                replay: false,
            },
        };
        if self.held_order(offer, node, request, &op) {
            self.set_active(offer, node, Active::Opening(request, None));
        } else if let Some(held) = self.held_mut(offer, node) {
            held.reach = Reach::ByPath;
        }
        Ran::Again
    }

    /// Ask for `node`'s open again, as `reopen` says, now its connection is
    /// back: an open that cannot be asked for leaves the file reached by
    /// path, and what it holds written lost.
    fn reopen_held(&mut self, offer: OfferId, node: NodeId, reopen: Reopen) {
        let request = self.next_request_id();
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return;
        };
        let path = volume.nodes.get(node).map(|found| found.path);
        let write = !volume.read_only;
        let Some(held) = volume.held.files.get_mut(&node.raw()) else {
            return;
        };
        let durability = match (reopen, held.durable) {
            (Reopen::Reclaim, Some(durable)) => Durability::Reclaim {
                create_guid: held.create_guid,
                file_id: durable.file_id,
            },
            _ => Durability::Ask {
                create_guid: held.create_guid,
                replay: true,
            },
        };
        let (handle, key) = (held.handle, held.key);
        let ordered = path.is_some_and(|path| {
            let op = FileOp::Hold {
                path: path.as_str(),
                handle,
                key,
                directory: false,
                write,
                durability,
            };
            self.held_order(offer, node, request, &op)
        });
        if ordered {
            self.set_active(offer, node, Active::Opening(request, Some(reopen)));
        } else {
            self.not_reopened(offer, node, DriverError::DeviceOffline);
        }
    }

    /// `node`'s open could not be had again: the file is reached by path,
    /// and what it held written, with nothing to send it on, is lost.
    fn not_reopened(&mut self, offer: OfferId, node: NodeId, error: DriverError) {
        let unsent = self.held_mut(offer, node).is_some_and(|held| {
            held.reopen = None;
            held.durable = None;
            held.reach = Reach::ByPath;
            !held.written.is_empty()
        });
        if unsent {
            self.lost(offer, node, error, Lost::All);
        }
    }

    /// Hold the call `ticket` in `node`'s work behind the close of the
    /// front's open on it.
    pub(super) fn park_after_close(
        &mut self,
        now: u64,
        offer: OfferId,
        node: NodeId,
        ticket: u64,
    ) -> Result<(), DriverError> {
        let held = self.held_mut(offer, node).ok_or(DriverError::DeviceFault)?;
        held.steps
            .try_reserve(2)
            .map_err(|_| DriverError::OutOfMemory)?;
        if !held.steps.contains(&Step::Close) {
            held.steps.push_back(Step::Close);
        }
        held.steps.push_back(Step::Call(ticket));
        self.pump_held(now, offer, node);
        Ok(())
    }

    /// Run the call `ticket` at the head of `node`'s work.
    fn run_call(&mut self, now: u64, offer: OfferId, node: NodeId, ticket: u64) -> Ran {
        let Some(op) = self
            .volumes
            .get(&offer.get())
            .and_then(|volume| volume.held_op(ticket))
        else {
            return Ran::Done;
        };
        match op {
            HeldOp::Read { offset, span } => self.read_held(offset, span, offer, node, ticket, now),
            HeldOp::Write { offset, span } => {
                self.write_held(offset, span, offer, node, ticket, now)
            }
            HeldOp::Truncate(size) => {
                if self
                    .held(offer, node)
                    .is_some_and(|held| held.is_open() && !held.written.is_empty())
                {
                    self.send_wave(offer, node);
                    return Ran::Again;
                }
                self.through(now, offer, node, ticket, Through::Resize(size))
            }
        }
    }

    /// Answer a read from what is cached, fetching the pages it lacks.
    fn read_held(
        &mut self,
        offset: u64,
        span: Span,
        offer: OfferId,
        node: NodeId,
        ticket: u64,
        now: u64,
    ) -> Ran {
        let Some(held) = self.held(offer, node) else {
            return Ran::Done;
        };
        if held.known().is_none() || self.pages.is_none() {
            return self.through(now, offer, node, ticket, Through::Read(offset, span));
        }
        let (size, generation, handle) = (held.size, held.generation, held.handle);
        let len = within(offset, span, size);
        if len == 0 {
            self.answer_held(offer, ticket, Ok(Answer::Count(0)));
            return Ran::Done;
        }
        let to = offset + len as u64;
        let Self {
            host, pages: cache, ..
        } = self;
        let Some(room) = host
            .window(offer, span)
            .and_then(|window| window.get_mut(..len))
        else {
            self.answer_held(offer, ticket, Err(DriverError::DeviceOffline));
            return Ran::Done;
        };
        room.fill(0);
        let mut missing: ArrayVec<u64, 2> = ArrayVec::new();
        for page in pages(offset, to) {
            let key = PageKey {
                offer: offer.get(),
                node: node.raw(),
                generation,
                page,
            };
            match cache.as_mut().and_then(|cache| cache.find(&(), &key)) {
                Some(found) => lay(room, offset, page, &found.0),
                // At most two pages, so all fit.
                None => {
                    let _ = missing.try_push(page);
                }
            }
        }
        if missing.is_empty() {
            self.finish_read(offer, node, ticket, offset, span, len);
            return Ran::Done;
        }
        let mut fetches = Vec::new();
        if fetches.try_reserve_exact(missing.len()).is_err() {
            self.answer_held(offer, ticket, Err(DriverError::OutOfMemory));
            return Ran::Done;
        }
        for page in missing {
            let request = self.next_request_id();
            let op = FileOp::ReadHeld {
                handle,
                offset: page * PAGE as u64,
                len: PAGE_LEN,
            };
            if !self.held_order(offer, node, request, &op) {
                self.answer_held(offer, ticket, Err(DriverError::DeviceOffline));
                return Ran::Done;
            }
            // Room was reserved above.
            fetches.push(Fetch {
                request,
                page,
                bytes: Vec::new(),
                done: false,
            });
        }
        self.set_active(offer, node, Active::Fetching(ticket, fetches));
        Ran::Waiting
    }

    /// Lay what `node` holds written over the read's bytes and answer it.
    fn finish_read(
        &mut self,
        offer: OfferId,
        node: NodeId,
        ticket: u64,
        offset: u64,
        span: Span,
        len: usize,
    ) {
        let Self { host, volumes, .. } = self;
        let held = volumes
            .get(&offer.get())
            .and_then(|volume| volume.held.files.get(&node.raw()));
        let room = host
            .window(offer, span)
            .and_then(|window| window.get_mut(..len));
        if let (Some(held), Some(room)) = (held, room) {
            held.written.overlay(offset, room);
        }
        let count = u32::try_from(len).unwrap_or(u32::MAX);
        self.answer_held(offer, ticket, Ok(Answer::Count(count)));
    }

    /// Keep a write's bytes when the lease and the bound let it; otherwise
    /// send it before answering.
    fn write_held(
        &mut self,
        offset: u64,
        span: Span,
        offer: OfferId,
        node: NodeId,
        ticket: u64,
        now: u64,
    ) -> Ran {
        let wall = self.host.wall_time();
        let Some(volume) = self.volumes.get(&offer.get()) else {
            return Ran::Done;
        };
        let Some(held) = volume.held.files.get(&node.raw()) else {
            return Ran::Done;
        };
        let leased = held.lease & WRITE != 0 && held.known().is_some();
        let more = held.written.new_blocks(offset, span.size()) * BLOCK_PINNED;
        let admitted = leased && volume.held.admits(more);
        if !admitted {
            if leased {
                volume.held.note_refusal();
            }
            // What is held goes first, so this write lands after it.
            if held.is_open() && !held.written.is_empty() {
                if leased {
                    volume.held.note_released();
                }
                self.send_wave(offer, node);
                return Ran::Again;
            }
            return self.through(now, offer, node, ticket, Through::Write(offset, span));
        }
        let end = offset.saturating_add(span.size() as u64);
        let Self {
            volumes,
            host,
            pages: cache,
            ..
        } = self;
        let (Some(volume), Some(bytes)) = (volumes.get_mut(&offer.get()), host.window(offer, span))
        else {
            return Ran::Done;
        };
        let Some(held) = volume.held.files.get_mut(&node.raw()) else {
            return Ran::Done;
        };
        let before = Unsent::of(&held.written);
        if held.written.write(offset, bytes, now).is_err() {
            return self.through(now, offer, node, ticket, Through::Write(offset, span));
        }
        let after = Unsent::of(&held.written);
        held.size = held.size.max(end);
        held.unsynced = true;
        if let Some(stat) = held.stat.as_mut() {
            stat.modified = wall;
            stat.changed = wall;
        }
        if let Some(cache) = cache.as_mut() {
            for page in pages(offset, end.max(offset + 1)) {
                cache.invalidate(&PageKey {
                    offer: offer.get(),
                    node: node.raw(),
                    generation: held.generation,
                    page,
                });
            }
        }
        volume.held.recount(before, after);
        self.stale_node(offer, node);
        let count = u32::try_from(span.size()).unwrap_or(u32::MAX);
        self.answer_held(offer, ticket, Ok(Answer::Count(count)));
        Ran::Done
    }

    /// Hand the call `ticket` to the server — on `node`'s open where there
    /// is one, by path where it was refused — answering it when the server
    /// does.
    fn through(
        &mut self,
        now: u64,
        offer: OfferId,
        node: NodeId,
        ticket: u64,
        through: Through,
    ) -> Ran {
        let via = self
            .held(offer, node)
            .filter(|held| held.is_open())
            .map(|held| held.handle);
        match self.order_through(now, offer, ticket, via, through) {
            Ok(()) => {
                self.set_active(offer, node, Active::Call(ticket));
                Ran::Waiting
            }
            Err(error) => {
                self.answer_held(offer, ticket, Err(error));
                Ran::Done
            }
        }
    }

    /// Answer the held call `ticket` with `result`.
    fn answer_held(
        &mut self,
        offer: OfferId,
        ticket: u64,
        result: Result<Answer<'_>, DriverError>,
    ) {
        let Some((ticket, op)) = self
            .volumes
            .get_mut(&offer.get())
            .and_then(|volume| volume.take_call(ticket))
        else {
            return;
        };
        self.served_answer(offer, ticket, op, result);
        self.release_spans(offer);
    }

    /// The held call `ticket` the server answered, having done `effect`: the
    /// file's work goes on.
    pub(super) fn held_call_done(
        &mut self,
        now: u64,
        offer: OfferId,
        node: NodeId,
        ticket: u64,
        effect: Effect,
    ) {
        let Self {
            volumes,
            pages: cache,
            ..
        } = self;
        let Some(held) = volumes
            .get_mut(&offer.get())
            .and_then(|volume| volume.held.files.get_mut(&node.raw()))
        else {
            return;
        };
        if matches!(held.active, Some(Active::Call(active) | Active::Fetching(active, _)) if active == ticket)
        {
            held.active = None;
        }
        match effect {
            Effect::Nothing => {}
            Effect::Wrote { offset, count } => {
                held.size = held.size.max(offset.saturating_add(count));
                held.unsynced = true;
                if let (Some(cache), true) = (cache.as_mut(), count > 0) {
                    for page in pages_of(offset, offset + count) {
                        cache.invalidate(&PageKey {
                            offer: offer.get(),
                            node: node.raw(),
                            generation: held.generation,
                            page,
                        });
                    }
                }
            }
            Effect::Resized(size) => {
                held.size = size;
                held.unsynced = true;
                held.generation = held.generation.wrapping_add(1);
            }
        }
        if effect != Effect::Nothing {
            self.stale_node(offer, node);
        }
        self.pump_held(now, offer, node);
    }

    /// Send every byte `node` holds written.
    fn send_wave(&mut self, offer: OfferId, node: NodeId) {
        if let Some(held) = self.held_mut(offer, node) {
            let replay = core::mem::take(&mut held.replay_writes);
            held.active = Some(Active::Sending(Wave {
                flight: ArrayVec::new(),
                cursor: 0,
                replay,
            }));
        }
        self.send_more(offer, node);
    }

    /// Put more of `node`'s wave in flight; once nothing is held, or out,
    /// the wave is done.
    fn send_more(&mut self, offer: OfferId, node: NodeId) {
        loop {
            let Some(held) = self.held_mut(offer, node) else {
                return;
            };
            let Held {
                handle,
                written,
                active,
                ..
            } = held;
            let Some(Active::Sending(wave)) = active else {
                return;
            };
            if wave.flight.is_full() {
                return;
            }
            // What a short write left goes once nothing else is out.
            let next = written.stretch(wave.cursor, IO_MAX).or_else(|| {
                wave.flight
                    .is_empty()
                    .then(|| written.stretch(0, IO_MAX))
                    .flatten()
            });
            let Some((offset, len)) = next else {
                if wave.flight.is_empty() {
                    *active = None;
                }
                return;
            };
            wave.cursor = offset + len as u64;
            let (handle, replay) = (*handle, wave.replay);
            // At most one call's worth, so it fits.
            let len32 = u32::try_from(len).unwrap_or(u32::MAX);
            let request = self.next_request_id();
            let op = FileOp::WriteHeld {
                handle,
                offset,
                len: len32,
                replay,
            };
            if self.stage_held(offer, node, request, offset, len)
                && self.held_order(offer, node, request, &op)
            {
                if let Some(Active::Sending(wave)) = self
                    .held_mut(offer, node)
                    .and_then(|held| held.active.as_mut())
                {
                    // Not full, as was seen above.
                    let _ = wave.flight.try_push(Flight {
                        offset,
                        request,
                        len: len32,
                    });
                }
            } else {
                self.lost(
                    offer,
                    node,
                    DriverError::DeviceOffline,
                    Lost::Stretch(offset, len),
                );
            }
        }
    }

    /// Hand the held bytes `[offset, offset + len)` of `node` to the worker,
    /// ahead of the write `request` orders, as few chunks as hold them.
    fn stage_held(
        &mut self,
        offer: OfferId,
        node: NodeId,
        request: u32,
        offset: u64,
        len: usize,
    ) -> bool {
        let Self { volumes, conns, .. } = self;
        let Some(volume) = volumes.get(&offer.get()) else {
            return false;
        };
        let Some(held) = volume.held.files.get(&node.raw()) else {
            return false;
        };
        let Some(link) = conns
            .get_mut(volume.conn)
            .and_then(|conn| conn.link.as_mut())
        else {
            return false;
        };
        let mut chunk: ArrayVec<&[u8], CHUNK_PARTS> = ArrayVec::new();
        let mut room = CHUNK_MAX;
        for mut part in held.written.parts(offset, len) {
            while !part.is_empty() {
                let (piece, rest) = part.split_at(part.len().min(room));
                // Staged below whenever it fills.
                let _ = chunk.try_push(piece);
                room -= piece.len();
                part = rest;
                if room == 0 || chunk.is_full() {
                    if link.stage(request, chunk.as_slice()).is_err() {
                        return false;
                    }
                    chunk.clear();
                    room = CHUNK_MAX;
                }
            }
        }
        chunk.is_empty() || link.stage(request, chunk.as_slice()).is_ok()
    }

    /// Let go of `what` of `node`'s written bytes, keeping the volume's
    /// count: how many bytes were held.
    fn let_written_go(&mut self, offer: OfferId, node: NodeId, what: Lost) -> usize {
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return 0;
        };
        let Some(held) = volume.held.files.get_mut(&node.raw()) else {
            return 0;
        };
        let before = Unsent::of(&held.written);
        let gone = match what {
            Lost::Stretch(offset, len) => held.written.let_go(offset, len),
            Lost::All => held.written.let_all_go(),
        };
        let after = Unsent::of(&held.written);
        volume.held.recount(before, after);
        gone
    }

    /// `what` of the bytes written to `node` was not taken by the server:
    /// say so, owe the volume's next flush the error, and cache nothing
    /// more of a file the kernel was told otherwise of.
    fn lost(&mut self, offer: OfferId, node: NodeId, error: DriverError, what: Lost) {
        let bytes = self.let_written_go(offer, node, what);
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return;
        };
        volume.held.lost.get_or_insert(error);
        if let Some(held) = volume.held.files.get_mut(&node.raw()) {
            held.stat = None;
            held.generation = held.generation.wrapping_add(1);
        }
        self.stale_node(offer, node);
        self.lost_bytes(offer, bytes);
    }

    /// Record that `bytes` written to `offer`'s share were never taken.
    pub(super) fn lost_bytes(&self, offer: OfferId, bytes: usize) {
        record(
            &self.sink,
            WRITE_BACK_LOST,
            Level::Warn,
            "smbfs: bytes written to a share were not taken",
            &[
                Field {
                    key: "offer",
                    value: FieldValue::UnsignedInt(offer.get()),
                },
                Field {
                    key: "bytes",
                    value: FieldValue::UnsignedInt(bytes as u64),
                },
            ],
        );
    }

    /// Have the server commit `node`'s bytes, for the flush `flush`.
    fn sync_held(&mut self, offer: OfferId, node: NodeId, flush: u32) -> Ran {
        let Some((open, unsynced, handle)) = self
            .held(offer, node)
            .map(|held| (held.is_open(), held.unsynced, held.handle))
        else {
            return Ran::Done;
        };
        if !open || !unsynced {
            self.synced(offer, flush);
            return Ran::Done;
        }
        let request = self.next_request_id();
        if self.held_order(offer, node, request, &FileOp::Sync { handle }) {
            self.set_active(offer, node, Active::Syncing(request, flush));
            return Ran::Waiting;
        }
        self.owe(offer, DriverError::DeviceOffline);
        self.synced(offer, flush);
        Ran::Done
    }

    /// Owe `offer`'s next flush `error`, unless it owes one already.
    fn owe(&mut self, offer: OfferId, error: DriverError) {
        if let Some(volume) = self.volumes.get_mut(&offer.get()) {
            volume.held.lost.get_or_insert(error);
        }
    }

    /// Acknowledge `node`'s lease break, keeping `bits`.
    fn ack_held(&mut self, offer: OfferId, node: NodeId, bits: u8) -> Ran {
        let Some(key) = self.held(offer, node).map(|held| held.key) else {
            return Ran::Done;
        };
        let request = self.next_request_id();
        if self.held_order(offer, node, request, &FileOp::Ack { key, state: bits }) {
            self.set_active(offer, node, Active::Acking(request, bits));
            return Ran::Waiting;
        }
        // An acknowledgment the connection cannot take is the server's to
        // time out: it breaks the lease itself.
        Ran::Done
    }

    /// Let `node`'s open go.
    fn close_held(&mut self, offer: OfferId, node: NodeId) -> Ran {
        let Some(handle) = self.held(offer, node).map(|held| held.handle) else {
            return Ran::Done;
        };
        let request = self.next_request_id();
        if self.held_order(offer, node, request, &FileOp::Close { handle }) {
            self.set_active(offer, node, Active::Closing(request));
            return Ran::Waiting;
        }
        // A close the connection cannot take is moot: the open goes with the
        // connection.
        if let Some(held) = self.held_mut(offer, node) {
            held.reach = Reach::Unopened;
        }
        Ran::Done
    }

    /// One file of the flush `flush` is committed.
    fn synced(&mut self, offer: OfferId, flush: u32) {
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return;
        };
        let Some(at) = volume
            .held
            .flushes
            .iter()
            .position(|(name, _)| *name == flush)
        else {
            return;
        };
        volume.held.flushes[at].1 -= 1;
        if volume.held.flushes[at].1 > 0 {
            return;
        }
        volume.held.flushes.swap_remove(at);
        let lost = volume.held.lost.take();
        if let Some((ticket, op)) = volume.take_flush(flush) {
            self.served_answer(offer, ticket, op, lost.map_or(Ok(Answer::Done), Err));
        }
    }

    /// Flush the volume: every held file's bytes sent and committed, the
    /// call `ticket` answered once they are, with any write the server
    /// refused since the last flush. `None` when nothing waits, and the
    /// call is answered at once.
    pub(super) fn flush_held(
        &mut self,
        now: u64,
        offer: OfferId,
        ticket: u64,
    ) -> Result<Option<()>, DriverError> {
        let volume = self
            .volumes
            .get_mut(&offer.get())
            .ok_or(DriverError::DeviceOffline)?;
        let mut waiting = Vec::new();
        for (&node, held) in &volume.held.files {
            if held.is_open()
                && (held.unsynced || !held.written.is_empty() || held.active.is_some())
            {
                waiting
                    .try_reserve(1)
                    .map_err(|_| DriverError::OutOfMemory)?;
                waiting.push(node);
            }
        }
        if waiting.is_empty() {
            return volume.held.lost.take().map_or(Ok(None), Err);
        }
        volume
            .held
            .flushes
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        volume
            .calls
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        for &node in &waiting {
            if let Some(held) = volume.held.files.get_mut(&node) {
                held.steps
                    .try_reserve(1)
                    .map_err(|_| DriverError::OutOfMemory)?;
            }
        }
        volume.held.next_flush = volume.held.next_flush.wrapping_add(1);
        let flush = volume.held.next_flush;
        volume.held.flushes.push((flush, waiting.len()));
        volume.flush_call(now, ticket, flush);
        for &node in &waiting {
            if let Some(held) = volume.held.files.get_mut(&node) {
                // Room was reserved above.
                held.steps.push_back(Step::Sync(flush));
            }
        }
        for node in waiting {
            self.pump_held(now, offer, NodeId::from_raw(node));
        }
        Ok(Some(()))
    }

    /// `offer`'s connection dropped: its held files stop, to go on once it is
    /// back.
    pub(super) fn suspend_held(&mut self, offer: OfferId) {
        let Self {
            volumes, routes, ..
        } = self;
        let Some(volume) = volumes.get_mut(&offer.get()) else {
            return;
        };
        for request in volume.held.ops.keys() {
            routes.remove(request);
        }
        volume.held.suspend();
    }

    /// `offer`'s connection is back: each held file's open is asked for
    /// again, and its work goes on behind it.
    pub(super) fn resume_held(&mut self, now: u64, offer: OfferId) {
        if let Some(volume) = self.volumes.get_mut(&offer.get()) {
            volume.held.paused = false;
        }
        self.expire_held(now, offer);
    }

    /// `offer`'s connection was not back within its grace window: what its
    /// held files hold written is lost, owed to the next flush.
    pub(super) fn lose_held(&mut self, offer: OfferId) {
        let lost = self
            .volumes
            .get_mut(&offer.get())
            .map_or(0, |volume| volume.held.give_up_all());
        if lost > 0 {
            self.lost_bytes(offer, lost);
        }
    }

    /// The kernel let go of its last hold on `node`: send what it holds, then
    /// let the open go.
    pub(super) fn unheld(&mut self, now: u64, offer: OfferId, node: NodeId) {
        let Some(held) = self.held_mut(offer, node) else {
            return;
        };
        if !held.steps.contains(&Step::Close) && held.steps.try_reserve(1).is_ok() {
            held.steps.push_back(Step::Close);
        }
        self.pump_held(now, offer, node);
    }

    /// The server is breaking the lease `key` on one of `offer`'s files;
    /// whether it is one of the volume's.
    pub(super) fn held_break(
        &mut self,
        now: u64,
        offer: OfferId,
        key: [u8; 16],
        new: u8,
        ack: bool,
    ) -> bool {
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            return false;
        };
        let Some(&node) = volume.held.keys.get(&key) else {
            return false;
        };
        let Some(held) = volume.held.files.get_mut(&node) else {
            return false;
        };
        held.lease = new;
        if new & READ == 0 {
            held.generation = held.generation.wrapping_add(1);
        }
        let step = if ack {
            Some(Step::Ack(new))
        } else if new & WRITE == 0 && !held.written.is_empty() {
            Some(Step::Wave)
        } else {
            None
        };
        if let Some(step) = step {
            if held.steps.try_reserve(1).is_ok() {
                // Ahead of whatever waits: the server, and another client,
                // wait on it.
                held.steps.push_front(step);
            }
        }
        self.pump_held(now, offer, NodeId::from_raw(node));
        true
    }

    /// Act on every held file of `offer` whose deadline has come by `now`:
    /// what it waited on past a call's deadline given up, work left waiting
    /// on its connection run, and its written bytes past the write-back
    /// window sent.
    pub(super) fn expire_held(&mut self, now: u64, offer: OfferId) {
        // Each turn moves the file's deadline past `now` or takes it away.
        while let Some(node) = self
            .volumes
            .get(&offer.get())
            .and_then(|volume| volume.held.due(now))
        {
            let node = NodeId::from_raw(node);
            let Some(held) = self.held_mut(offer, node) else {
                return;
            };
            let waiting =
                held.reopen.is_some() || held.interrupted.is_some() || !held.steps.is_empty();
            if held.active.is_some() {
                self.give_up(offer, node);
            } else if waiting {
                // The pump below runs what waits.
            } else if held.steps.try_reserve(1).is_ok() {
                held.steps.push_back(Step::Wave);
            } else {
                held.not_before = now.saturating_add(WRITE_BACK_NS);
            }
            self.pump_held(now, offer, node);
        }
    }

    /// Stop waiting on what the server left `node` unanswered: written bytes
    /// it held are lost, and the head call was answered at its deadline.
    fn give_up(&mut self, offer: OfferId, node: NodeId) {
        let Some(held) = self.held_mut(offer, node) else {
            return;
        };
        let Some(active) = held.active.take() else {
            return;
        };
        match active {
            Active::Sending(_) => self.lost(offer, node, DriverError::TimedOut, Lost::All),
            Active::Syncing(_, flush) => {
                self.owe(offer, DriverError::TimedOut);
                self.synced(offer, flush);
            }
            Active::Opening(_, None) => {
                if let Some(held) = self.held_mut(offer, node) {
                    held.reach = Reach::ByPath;
                }
            }
            Active::Opening(_, Some(_)) => self.not_reopened(offer, node, DriverError::TimedOut),
            Active::Closing(_) => {
                if let Some(held) = self.held_mut(offer, node) {
                    held.reach = Reach::Unopened;
                }
            }
            Active::Fetching(..) | Active::Call(_) | Active::Acking(..) => {}
        }
    }

    /// Act on the report for `node`'s request `request`.
    pub(super) fn held_report(
        &mut self,
        now: u64,
        offer: OfferId,
        node: NodeId,
        request: u32,
        report: &Report<'_>,
    ) -> Result<(), Lie> {
        if !matches!(report, Report::Data { .. }) {
            if let Some(volume) = self.volumes.get_mut(&offer.get()) {
                volume.held.ops.remove(&request);
            }
            self.routes.remove(&request);
        }
        let Some(held) = self.held_mut(offer, node) else {
            return Ok(());
        };
        match (&mut held.active, report) {
            (
                Some(Active::Opening(asked, reopen)),
                Report::Held {
                    stat,
                    lease,
                    file_id,
                    durable_ms,
                    ..
                },
            ) if *asked == request => {
                let reopen = *reopen;
                held.reach = Reach::Open;
                held.lease = *lease;
                // What is held written lands past the server's end too.
                held.size = stat.size.max(held.written.end());
                held.stat = Some(*stat);
                held.active = None;
                held.durable = match (durable_ms, held.durable) {
                    (Some(timeout_ms), _) => Some(Durable {
                        file_id: *file_id,
                        timeout_ms: *timeout_ms,
                    }),
                    (None, Some(durable)) if reopen == Some(Reopen::Reclaim) => Some(Durable {
                        file_id: *file_id,
                        ..durable
                    }),
                    (None, _) => None,
                };
                if reopen.is_some() {
                    held.reopen = None;
                    held.replay_writes = !held.written.is_empty();
                    // A lease not kept across the gap leaves nothing cached
                    // worth trusting.
                    if reopen == Some(Reopen::Replay) || *lease & READ == 0 {
                        held.generation = held.generation.wrapping_add(1);
                    }
                }
            }
            (Some(Active::Opening(asked, reopen)), Report::Refused { error, .. })
                if *asked == request =>
            {
                let reopen = *reopen;
                held.active = None;
                match reopen {
                    None => held.reach = Reach::ByPath,
                    // The server let the durable open go: ask afresh.
                    Some(Reopen::Reclaim) => {
                        held.reopen = Some(Reopen::Replay);
                        held.durable = None;
                    }
                    Some(Reopen::Replay) => self.not_reopened(offer, node, driver_error(*error)),
                }
            }
            (
                Some(Active::Fetching(..)),
                Report::Data { .. } | Report::Done { .. } | Report::Refused { .. },
            ) => {
                return self.fetched(now, offer, node, request, report);
            }
            (Some(Active::Sending(wave)), Report::Done { .. } | Report::Refused { .. })
                if wave.flight.iter().any(|flight| flight.request == request) =>
            {
                return self.sent_run(now, offer, node, request, report);
            }
            (Some(Active::Syncing(asked, flush)), Report::Done { .. } | Report::Refused { .. })
                if *asked == request =>
            {
                let flush = *flush;
                let refused = match report {
                    Report::Refused { error, .. } => Some(driver_error(*error)),
                    _ => None,
                };
                held.active = None;
                held.unsynced &= refused.is_some();
                if let Some(error) = refused {
                    self.owe(offer, error);
                }
                self.synced(offer, flush);
            }
            (
                Some(Active::Acking(asked, _) | Active::Closing(asked)),
                Report::Done { .. } | Report::Refused { .. },
            ) if *asked == request => {
                if matches!(held.active, Some(Active::Closing(_))) {
                    held.reach = Reach::Unopened;
                }
                held.active = None;
            }
            // A report for work given up on: nothing waits for it.
            _ => return Ok(()),
        }
        self.pump_held(now, offer, node);
        Ok(())
    }

    /// Part or the end of one page a head read waits for.
    fn fetched(
        &mut self,
        now: u64,
        offer: OfferId,
        node: NodeId,
        request: u32,
        report: &Report<'_>,
    ) -> Result<(), Lie> {
        let Some(held) = self.held_mut(offer, node) else {
            return Ok(());
        };
        let Some(Active::Fetching(ticket, fetches)) = &mut held.active else {
            return Ok(());
        };
        let ticket = *ticket;
        let Some(fetch) = fetches.iter_mut().find(|fetch| fetch.request == request) else {
            return Ok(());
        };
        match report {
            Report::Data { chunk, .. } => {
                if fetch.done || fetch.bytes.len() + chunk.len() > PAGE {
                    return Err(Lie);
                }
                if fetch.bytes.try_reserve(chunk.len()).is_err() {
                    held.active = None;
                    self.answer_held(offer, ticket, Err(DriverError::OutOfMemory));
                    self.pump_held(now, offer, node);
                    return Ok(());
                }
                fetch.bytes.extend_from_slice(chunk);
                return Ok(());
            }
            Report::Done { count, .. } => {
                if usize::try_from(*count).ok() != Some(fetch.bytes.len()) {
                    return Err(Lie);
                }
                fetch.done = true;
            }
            Report::Refused { error, .. } => {
                let error = driver_error(*error);
                held.active = None;
                self.answer_held(offer, ticket, Err(error));
                self.pump_held(now, offer, node);
                return Ok(());
            }
            _ => return Err(Lie),
        }
        if !fetches.iter().all(|fetch| fetch.done) {
            return Ok(());
        }
        let (size, generation) = (held.size, held.generation);
        let Some(Active::Fetching(_, fetches)) = held.active.take() else {
            return Ok(());
        };
        let op = self
            .volumes
            .get(&offer.get())
            .and_then(|volume| volume.held_op(ticket));
        if let Some(HeldOp::Read { offset, span }) = op {
            let len = within(offset, span, size);
            if let Some(room) = self
                .host
                .window(offer, span)
                .and_then(|window| window.get_mut(..len))
            {
                for fetch in &fetches {
                    lay(room, offset, fetch.page, &fetch.bytes);
                }
            }
            self.finish_read(offer, node, ticket, offset, span, len);
        }
        if let Some(cache) = self.pages.as_mut() {
            for fetch in fetches {
                let key = PageKey {
                    offer: offer.get(),
                    node: node.raw(),
                    generation,
                    page: fetch.page,
                };
                cache.retain(&(), key, Page(fetch.bytes));
            }
        }
        self.pump_held(now, offer, node);
        Ok(())
    }

    /// One write of `node`'s wave was taken, in part or whole, or refused.
    fn sent_run(
        &mut self,
        now: u64,
        offer: OfferId,
        node: NodeId,
        request: u32,
        report: &Report<'_>,
    ) -> Result<(), Lie> {
        let Some(held) = self.held_mut(offer, node) else {
            return Ok(());
        };
        let Some(Active::Sending(wave)) = &mut held.active else {
            return Ok(());
        };
        let at = wave
            .flight
            .iter()
            .position(|flight| flight.request == request)
            .ok_or(Lie)?;
        let flight = wave.flight.as_slice()[at];
        let len = flight.len as usize;
        let taken = match *report {
            Report::Done { count, .. } if count <= flight.len => {
                usize::try_from(count).map_err(|_| Lie)?
            }
            Report::Refused { error, .. } => {
                wave.flight.swap_remove(at);
                self.lost(
                    offer,
                    node,
                    driver_error(error),
                    Lost::Stretch(flight.offset, len),
                );
                self.send_more(offer, node);
                self.pump_held(now, offer, node);
                return Ok(());
            }
            _ => return Err(Lie),
        };
        wave.flight.swap_remove(at);
        // Progress, so the wave's deadline measures a server gone quiet.
        held.active_since = now;
        if taken == 0 {
            self.lost(
                offer,
                node,
                DriverError::NoSpace,
                Lost::Stretch(flight.offset, len),
            );
        } else {
            // A short write leaves the rest held, to go again.
            self.let_written_go(offer, node, Lost::Stretch(flight.offset, taken));
        }
        self.send_more(offer, node);
        self.pump_held(now, offer, node);
        Ok(())
    }
}

/// The pages of a file's bytes `[from, to)`, an empty range taking the page
/// of `from`.
fn pages_of(from: u64, to: u64) -> core::ops::RangeInclusive<u64> {
    pages(from, to.max(from + 1))
}

#[cfg(test)]
#[path = "held_tests.rs"]
pub(super) mod tests;
