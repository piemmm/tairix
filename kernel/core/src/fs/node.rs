//! Open file descriptions bound to a pinned node (`plans/OPENFILE.md`).
//!
//! A descriptor of a volume's node holds a [`NodeHandle`]: one driver pin on
//! that node, the projection it was opened through, and whose identity its
//! byte operations run under. Nothing after open looks at a path.
//!
//! A pin's last [`NodeRef`] may drop in any context, the address-space lock
//! held included, so the drop only queues the release on the registration's
//! [`VolumeNodes`], in the [`ReleaseSlot`] reserved before the pin was taken.
//! A close tells the driver when its lock is free, and the write-back flusher
//! otherwise ([`VolumeNodes::reap`]) — save a registration
//! [reaped inline](VolumeNodes::reaped_inline), whose driver answers from a
//! server the flusher must never wait on: the next operation on its volume
//! tells it. The queue's spin lock is a leaf: only the kernel heap's own lock
//! is taken beneath it, by an admission growing the reservation.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use core::fmt;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tairix_abi::driver::filesystem::{FilesystemRead, NodeId, NodeKind};
use tairix_abi::fsserve::SERVED_LANES_MAX;
use tairix_abi::{CapabilityQuery, DriverError, Errno};
use tairix_caps::CapabilitySet;
use tairix_inline::ArrayVec;
use tairix_sync::SpinLock;

use super::mount::Projection;
use super::VfsError;
use crate::sleeplock::{SleepGuard, SleepLock};

/// The kernel's record of one registration of a volume's driver.
///
/// The instance number is never reused, though the driver handle the volume
/// was registered under may be. Detach, retraction, recovery and a clean
/// surprise removal retire a registration, and a descriptor of a retired one
/// answers [`VfsError::Stale`]; the stand-in for a volume that vanished with
/// work unpublished, or whose server was lost, takes the registration over
/// instead.
pub struct VolumeNodes {
    instance: u64,
    retired: AtomicBool,
    /// The operations on the volume reap its releases, never the flusher.
    inline: bool,
    /// A release was queued that has not been reaped.
    reap_due: AtomicBool,
    queue: SpinLock<ReleaseQueue>,
}

/// The pins a registration's descriptors hold, and the releases waiting for
/// the driver.
///
/// `outstanding + pending.len() <= pending.capacity()`: taking a pin reserved
/// its release's slot, so a release never allocates.
struct ReleaseQueue {
    outstanding: usize,
    pending: VecDeque<NodeId>,
}

impl VolumeNodes {
    /// A registration numbered `instance`, holding no pin, whose releases the
    /// write-back flusher reaps.
    #[must_use]
    pub fn new(instance: u64) -> Self {
        Self::with_reaping(instance, false)
    }

    /// A registration numbered `instance`, holding no pin, whose releases the
    /// operations on the volume reap: its driver answers from a user-space
    /// server, which the write-back flusher, serving every volume, must never
    /// wait on.
    #[must_use]
    pub fn reaped_inline(instance: u64) -> Self {
        Self::with_reaping(instance, true)
    }

    fn with_reaping(instance: u64, inline: bool) -> Self {
        Self {
            instance,
            retired: AtomicBool::new(false),
            inline,
            reap_due: AtomicBool::new(false),
            queue: SpinLock::new(ReleaseQueue {
                outstanding: 0,
                pending: VecDeque::new(),
            }),
        }
    }

    /// Whether the operations on the volume reap its releases, rather than
    /// the write-back flusher.
    #[must_use]
    pub fn reaps_inline(&self) -> bool {
        self.inline
    }

    /// The registration's number.
    #[must_use]
    pub fn instance(&self) -> u64 {
        self.instance
    }

    /// Whether the registration is retired.
    ///
    /// Asked under the driver's lock: a retirement made under it is seen by
    /// every operation that takes the lock after, and one made without it,
    /// as a surprise removal's is, leaves an operation already past the check
    /// to fail against the departed device.
    #[must_use]
    pub fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    /// Retire the registration: no descriptor of it reaches the volume again.
    pub fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }

    /// Reserve the release slot of a pin about to be taken.
    ///
    /// # Errors
    ///
    /// [`VfsError::OutOfMemory`] when the slot cannot be reserved; nothing is
    /// reserved, and no pin may be taken.
    pub fn admit(self: &Arc<Self>) -> Result<ReleaseSlot, VfsError> {
        let mut queue = self.queue.lock();
        let more = queue
            .outstanding
            .checked_add(1)
            .ok_or(VfsError::OutOfMemory)?;
        queue
            .pending
            .try_reserve(more)
            .map_err(|_| VfsError::OutOfMemory)?;
        queue.outstanding = more;
        Ok(ReleaseSlot {
            volume: Arc::clone(self),
            taken: false,
        })
    }

    fn give_back(&self) {
        self.queue.lock().outstanding -= 1;
    }

    /// How many release slots are reserved: the pins the registration's
    /// descriptors hold, and any being taken. Under the driver's lock, where
    /// pins are taken, that is the pins held.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.queue.lock().outstanding
    }

    /// Whether a release waits for a reap, without taking the mark: the
    /// write-back flusher asks before it tries the driver's lock, so a
    /// volume it finds busy stays due for the lock's holder to report.
    #[must_use]
    pub fn is_reap_due(&self) -> bool {
        self.reap_due.load(Ordering::Acquire)
    }

    /// Whether a release was queued since this was last asked. Taken under
    /// the driver's lock, by whoever then reaps.
    pub fn take_reap_due(&self) -> bool {
        self.reap_due.swap(false, Ordering::AcqRel)
    }

    /// Tell `fs`, whose lock the caller holds, of every queued release. Each
    /// is taken from the queue on its own, so a reap allocates nothing and
    /// the driver is never called under the queue's lock.
    ///
    /// A retired registration's releases are dropped: its driver let every
    /// pin go before it was retired, or is gone. One reaped inline whose
    /// driver answers [`DriverError::Busy`] — a server that has not answered
    /// an earlier call on its holds, so the release was never sent — keeps
    /// it, first in line, for the next operation's reap.
    pub fn reap<F: FilesystemRead + ?Sized>(&self, fs: &mut F) {
        if self.is_retired() {
            self.queue.lock().pending.clear();
            return;
        }
        loop {
            let next = self.queue.lock().pending.pop_front();
            let Some(pinned) = next else {
                return;
            };
            match fs.unpin(pinned) {
                // Back in the slot it was just taken from, so nothing allocates.
                Err(DriverError::Busy) if self.inline => {
                    self.queue.lock().pending.push_front(pinned);
                    self.reap_due.store(true, Ordering::Release);
                    return;
                }
                // The hold goes whatever else this answers; a node the driver
                // failed to free is left to the format's own checker.
                _ => {}
            }
        }
    }

    /// Before an operation on a volume reaped inline, `fs` locked by the
    /// caller: tell it of the releases queued since the last.
    pub fn reap_inline<F: FilesystemRead + ?Sized>(&self, fs: &mut F) {
        // A load first: every operation asks, and almost none finds one due.
        if self.inline && self.is_reap_due() && self.take_reap_due() {
            self.reap(fs);
        }
    }

    /// Queue `pinned`'s release, answering whether the write-back flusher
    /// must be woken for it: only when it made a registration the flusher
    /// reaps due, so a burst of releases between two of its passes wakes it
    /// once.
    fn queue_release(&self, pinned: NodeId) -> bool {
        {
            let mut queue = self.queue.lock();
            queue.outstanding -= 1;
            queue.pending.push_back(pinned);
        }
        !self.reap_due.swap(true, Ordering::AcqRel) && !self.inline
    }
}

/// A registered volume's driver behind the lock that serialises it, beside the
/// registration whose releases it reaps.
///
/// A volume with other writers is served by several lanes, each a driver
/// held alone for an operation, and the lock hands an operation any free
/// one: it tries every lane from a rotating start, then queues on one. A
/// volume the kernel alone writes has one lane, held as before.
///
/// The write-back flusher only tries a volume's lock, leaving a busy volume
/// due, so every hold that ends while releases wait wakes it again: a long
/// operation on one volume never stalls the flusher's work on another.
pub struct DriverLock<F> {
    nodes: Arc<VolumeNodes>,
    first: SleepLock<F>,
    /// The lanes past the first: none for a volume with one writer.
    more: ArrayVec<SleepLock<F>, { SERVED_LANES_MAX - 1 }>,
    /// Where the next search for a free lane starts.
    next: AtomicUsize,
}

impl<F> DriverLock<F> {
    /// `driver`, serving the registration `nodes`.
    #[must_use]
    pub fn new(driver: F, nodes: Arc<VolumeNodes>) -> Self {
        Self {
            nodes,
            first: SleepLock::new(driver),
            more: ArrayVec::new(),
            next: AtomicUsize::new(0),
        }
    }

    /// `lanes`, each a driver of one volume serving the registration `nodes`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for no lanes or more than [`SERVED_LANES_MAX`].
    pub fn lanes(
        lanes: impl IntoIterator<Item = F>,
        nodes: Arc<VolumeNodes>,
    ) -> Result<Self, Errno> {
        let mut lanes = lanes.into_iter();
        let mut lock = Self::new(lanes.next().ok_or(Errno::OutOfRange)?, nodes);
        for lane in lanes {
            lock.more
                .try_push(SleepLock::new(lane))
                .map_err(|_| Errno::OutOfRange)?;
        }
        Ok(lock)
    }

    /// How many lanes serve the volume.
    #[must_use]
    pub fn lane_count(&self) -> usize {
        1 + self.more.len()
    }

    /// Lane `index`, which callers keep below [`Self::lane_count`].
    fn lane(&self, index: usize) -> &SleepLock<F> {
        index
            .checked_sub(1)
            .and_then(|at| self.more.get(at))
            .unwrap_or(&self.first)
    }

    /// Hold a lane of the driver, parking until one is free.
    #[track_caller]
    pub fn lock(&self) -> DriverGuard<'_, F> {
        if self.more.is_empty() {
            return self.guard(self.first.lock());
        }
        let count = self.lane_count();
        let start = self.next.fetch_add(1, Ordering::Relaxed) % count;
        for step in 0..count {
            if let Some(guard) = self.lane((start + step) % count).try_lock() {
                return self.guard(guard);
            }
        }
        self.guard(self.lane(start).lock())
    }

    /// Hold a lane of the driver if one is free this instant.
    #[track_caller]
    pub fn try_lock(&self) -> Option<DriverGuard<'_, F>> {
        // A loop, not a closure: the lane's own caller site is this caller's
        // only when the call is made directly.
        for index in 0..self.lane_count() {
            if let Some(guard) = self.lane(index).try_lock() {
                return Some(self.guard(guard));
            }
        }
        None
    }

    /// Hold every lane, so nothing else is in flight on the volume. Taken in
    /// order, so two callers taking them all never deadlock, and one taking
    /// a single lane never holds it while waiting for another.
    #[track_caller]
    pub fn lock_all(&self) -> AllLanes<'_, F> {
        let mut more = ArrayVec::new();
        for lane in &self.more {
            // Never full: it holds one guard per lane past the first.
            let _ = more.try_push(lane.lock());
        }
        AllLanes {
            first: self.first.lock(),
            more,
            _reported: ReapReport(&self.nodes),
        }
    }

    fn guard<'a>(&'a self, guard: SleepGuard<'a, F>) -> DriverGuard<'a, F> {
        DriverGuard {
            guard,
            _reported: ReapReport(&self.nodes),
        }
    }

    /// The registration the driver serves.
    #[must_use]
    pub fn nodes(&self) -> &Arc<VolumeNodes> {
        &self.nodes
    }
}

/// Every lane of a [`DriverLock`], held.
pub struct AllLanes<'a, F> {
    // Dropped before `_reported`, so the wake follows the release.
    first: SleepGuard<'a, F>,
    more: ArrayVec<SleepGuard<'a, F>, { SERVED_LANES_MAX - 1 }>,
    _reported: ReapReport<'a>,
}

impl<'a, F> AllLanes<'a, F> {
    /// The first lane: the one an operation on the quiet volume goes through.
    pub fn first(&mut self) -> &mut F {
        &mut self.first
    }

    /// Every lane, in order.
    pub fn each<'s>(&'s mut self) -> impl Iterator<Item = &'s mut F> + use<'s, 'a, F> {
        core::iter::once(&mut *self.first).chain(self.more.iter_mut().map(|guard| &mut **guard))
    }
}

impl<F> fmt::Debug for DriverLock<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DriverLock")
            .field("nodes", &self.nodes)
            .finish_non_exhaustive()
    }
}

/// A hold of a [`DriverLock`].
pub struct DriverGuard<'a, F: ?Sized> {
    // Dropped before `_reported`, so the wake follows the release.
    guard: SleepGuard<'a, F>,
    _reported: ReapReport<'a>,
}

impl<F: ?Sized> Deref for DriverGuard<'_, F> {
    type Target = F;

    fn deref(&self) -> &F {
        &self.guard
    }
}

impl<F: ?Sized> DerefMut for DriverGuard<'_, F> {
    fn deref_mut(&mut self) -> &mut F {
        &mut self.guard
    }
}

/// Wakes the write-back flusher when a hold ends with releases it reaps still
/// waiting.
struct ReapReport<'a>(&'a VolumeNodes);

impl Drop for ReapReport<'_> {
    fn drop(&mut self) {
        if self.0.is_reap_due() && !self.0.inline {
            crate::waitq::request_writeback_reap();
        }
    }
}

/// The release slot of one pin about to be taken, reserved before the driver
/// is asked so the pin's release allocates nothing. [`NodeRef::new`] consumes
/// it; dropped unconsumed, when the driver refuses the pin, it gives the slot
/// back.
#[must_use = "a slot not consumed by `NodeRef::new` is given back"]
pub struct ReleaseSlot {
    volume: Arc<VolumeNodes>,
    taken: bool,
}

impl Drop for ReleaseSlot {
    fn drop(&mut self) {
        if !self.taken {
            self.volume.give_back();
        }
    }
}

impl fmt::Debug for ReleaseSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReleaseSlot")
            .field("instance", &self.volume.instance)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for VolumeNodes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VolumeNodes")
            .field("instance", &self.instance)
            .field("retired", &self.is_retired())
            .finish_non_exhaustive()
    }
}

/// One pin the kernel holds on a node, shared by every [`NodeRef`] cloned
/// from the first. Until the last goes, the pinned id names the node through
/// renames, writes and the loss of its last name.
pub struct PinnedNode {
    volume: Arc<VolumeNodes>,
    pinned: NodeId,
    kind: NodeKind,
}

impl Drop for PinnedNode {
    fn drop(&mut self) {
        if self.volume.queue_release(self.pinned) {
            crate::waitq::request_writeback_reap();
        }
    }
}

impl fmt::Debug for PinnedNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinnedNode")
            .field("instance", &self.volume.instance)
            .field("pinned", &self.pinned.raw())
            .field("kind", &self.kind)
            .finish()
    }
}

/// A shared reference to one [`PinnedNode`]; clones share the pin. Two
/// references are equal when they name one node of one registration.
#[derive(Clone, Debug)]
pub struct NodeRef(Arc<PinnedNode>);

impl NodeRef {
    /// The pin the driver answered as `pinned` for a node of `kind`, in the
    /// release `slot` [admitted](VolumeNodes::admit) for it before the driver
    /// was asked.
    ///
    /// # Errors
    ///
    /// [`VfsError::OutOfMemory`] when the reference cannot be allocated; the
    /// pin's release is queued all the same, in the slot.
    pub fn new(mut slot: ReleaseSlot, pinned: NodeId, kind: NodeKind) -> Result<Self, VfsError> {
        slot.taken = true;
        Arc::try_new(PinnedNode {
            volume: Arc::clone(&slot.volume),
            pinned,
            kind,
        })
        .map(Self)
        .map_err(|_| VfsError::OutOfMemory)
    }

    /// The id the driver names the node by while it is pinned.
    #[must_use]
    pub fn pinned(&self) -> NodeId {
        self.0.pinned
    }

    /// The node's kind.
    #[must_use]
    pub fn kind(&self) -> NodeKind {
        self.0.kind
    }

    /// The number of the registration the pin was taken on.
    #[must_use]
    pub fn instance(&self) -> u64 {
        self.0.volume.instance
    }

    /// The registration the pin was taken on.
    #[must_use]
    pub fn volume(&self) -> &Arc<VolumeNodes> {
        &self.0.volume
    }
}

impl PartialEq for NodeRef {
    fn eq(&self, other: &Self) -> bool {
        self.instance() == other.instance() && self.pinned() == other.pinned()
    }
}

impl Eq for NodeRef {}

/// Whose identity a descriptor's byte operations run under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Authority {
    /// The opener's own descriptor: each operation runs as the process making
    /// it, so a capability narrowed while it is open stops working at once.
    Holder,
    /// An identity captured when the descriptor was conferred at spawn or
    /// granted.
    Frozen(FrozenAuthority),
}

/// The identity a conferred or granted descriptor carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrozenAuthority {
    /// The conferring parent's, or the first grantor's, uid.
    pub uid: u32,
    /// Their effective capability set when they handed the descriptor on.
    pub caps: CapabilitySet,
    /// What an `fd_grant` hand-off attenuates; `None` for a spawn's.
    pub grant: Option<Grant>,
}

/// What an `fd_grant` hand-off attenuates.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Grant {
    /// The highest length the holder may write or truncate the file to.
    pub write_ceiling: Option<u64>,
}

/// The identity one byte operation runs under, built by the syscall layer
/// from the descriptor's [`Authority`] and its caller.
#[derive(Copy, Clone)]
pub struct Acting<'a> {
    /// The uid the operation runs as.
    pub uid: u32,
    /// The capability set it runs with.
    pub caps: &'a dyn CapabilityQuery,
    /// Whether the descriptor was granted, which bars the owner's exemption.
    pub granted: bool,
}

/// What a descriptor of a volume's node holds.
#[derive(Clone, Debug)]
pub struct NodeHandle {
    /// The pin.
    pub node: NodeRef,
    /// The projection the node was opened through.
    pub projection: Arc<Projection>,
    /// Whose identity its byte operations run under.
    pub authority: Authority,
    /// The path it was opened as: a label, never resolved.
    pub opened_as: Arc<String>,
    /// Whether the node is the projection's floor, whose listing shows the
    /// projections attached beneath it.
    pub at_floor: bool,
}

/// Where a relative path is resolved from. An absolute path ignores it.
#[derive(Clone, Copy, Debug)]
pub enum Start<'a> {
    /// The view root, before a root volume is mounted.
    Root,
    /// A directory the caller holds: its working directory or a descriptor.
    Dir(&'a NodeHandle),
    /// The catalog the caller holds or works in.
    Catalog(&'a Projection),
}

/// A path, and where it starts when relative.
#[derive(Clone, Copy, Debug)]
pub struct At<'a> {
    /// Where a relative `path` starts.
    pub start: Start<'a>,
    /// A `/`-view or relative path.
    pub path: &'a str,
}

/// A process's working directory.
#[derive(Clone, Debug)]
pub enum Cwd {
    /// The view root.
    Root,
    /// A directory, pinned for as long as it is the working directory.
    Dir(NodeHandle),
    /// A catalog of attached volumes.
    Catalog(Arc<Projection>),
}

impl Cwd {
    /// Where a path relative to this working directory starts.
    #[must_use]
    pub fn start(&self) -> Start<'_> {
        match self {
            Self::Root => Start::Root,
            Self::Dir(handle) => Start::Dir(handle),
            Self::Catalog(catalog) => Start::Catalog(catalog),
        }
    }
}

#[cfg(test)]
#[path = "node_tests.rs"]
mod tests;
