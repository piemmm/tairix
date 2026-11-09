//! The kernel wait-set registry: caller-owned objects that multiplex the
//! readiness of several heterogeneous event sources (`plans/USB.md` — the
//! asynchronous host-controller event loop).
//!
//! A wait-set is the scalable analogue of `epoll`/`kqueue`. A service that
//! must react to many event streams in one process — for the USB host
//! controller, incoming URB-submit IPC calls on the per-interface endpoints it
//! serves *and* its controller's completion interrupt — registers each source
//! once and then blocks on the set, instead of busy-polling (the charter
//! forbids spinning a core) or re-supplying a per-wait array (which would cap
//! the number of sources at a fixed ceiling). Membership lives in a growable
//! [`Vec`], bounded by the owner's `wait-set-members` limit rather than a
//! hand-picked constant, and each set by its `wait-sets` limit: the charges a
//! set holds for both are returned when it goes.
//!
//! # What lives here, and what does not
//!
//! This module is *pure registry mechanics*, like [`crate::callreg`]: it owns
//! the map from a minted handle to a set's `{owner, members}` and enforces
//! that only the set's owner may read or mutate it. It deliberately does **not**
//! resolve a member's underlying resource (an IPC endpoint, an IRQ line) or
//! test its readiness — that needs the call-endpoint registry and the IRQ
//! table, which the syscall handler in [`crate::syscalls`] holds. The handler
//! owner-checks the *resource* before adding a member and re-checks it while
//! scanning for readiness; this registry only owner-checks the *set*. It also
//! defines what a wait leaves its thread owing (`RoomDebt`), which each
//! thread keeps on its own gate: a port's slot and the budget release that
//! wait kept, which its next call other than that send, or its end, lets go.
//!
//! # Why a global
//!
//! Like [`crate::callreg`] and [`crate::waitq`], the registry is global pure
//! data behind a [`SpinLock`] (never a `static mut`, so not the global mutable
//! static the charter forbids): the wait-set is created, modified, and waited
//! on from the syscall handler, and torn down from the task-exit / driver-
//! unload path, none of which own the others. A global keyed by the minted
//! handle is the natural rendezvous and avoids threading another map through
//! `KernelState`'s cross-crate wiring.
//!
//! The registry's lock is held only to find a set: each set has a lock of its
//! own, so one process's waits never queue behind another's, and a process's
//! teardown finds its sets through an index of their owners rather than by
//! walking every process's.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use tairix_abi::{Errno, WaitSourceKind};
use tairix_kernel_ipc::CallEndpoint;
use tairix_kernel_sec::Charge;
use tairix_sync::SpinLock;

use crate::waitq::WakeKey;

/// One registered source of a wait-set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Member {
    /// The kind of resource this member observes.
    pub kind: WaitSourceKind,
    /// The resource id: an IPC call-endpoint id for [`WaitSourceKind::Endpoint`],
    /// or an [`tairix_abi::IrqHandle`] raw value for [`WaitSourceKind::Irq`].
    pub id: u64,
    /// The caller's opaque token, reported back by a successful wait when this
    /// member is the one found ready.
    pub token: u64,
    /// For a [`WaitSourceKind::File`] or [`WaitSourceKind::DirWatch`]
    /// member: the id the node it watches is pinned under, its watch table's
    /// key for it. `0` for every other kind.
    pub node: u64,
    /// The edge an edge-triggered member last reported: a `File` member's node
    /// generation, a `SystemNotice` member's topic generation. Unused (`0`) for
    /// every other kind.
    pub observed: u64,
    /// What a `DirWatch` member last reported and how it paces the next;
    /// default for every other kind.
    pub pacing: crate::fswatch::Pacing,
    /// For a `File` or `DirWatch` member: the watch table of the volume its node
    /// is on, held for the member's life so a volume that leaves still
    /// reports its departure to the member rather than vanishing from it.
    pub table: Option<WatchTable>,
    /// For a `File` member: the pin on its node, held as an open descriptor
    /// holds it, so the pinned id names that node for the member's whole life
    /// rather than whatever the driver next numbers so.
    pub pin: Option<crate::fs::NodeRef>,
}

/// Whether a wait on `members` hears requests: only one holding an endpoint
/// or a port joins the queue a post wakes, so a post wakes no other wait.
#[must_use]
pub fn hears_requests(members: &[Member]) -> bool {
    members
        .iter()
        .any(|m| matches!(m.kind, WaitSourceKind::Endpoint | WaitSourceKind::Port))
}

/// A volume's watch table held by a member; two are equal only when they are
/// the same table.
#[derive(Clone)]
pub struct WatchTable(pub Arc<crate::fswatch::VolumeWatch>);

impl PartialEq for WatchTable {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for WatchTable {}

impl core::fmt::Debug for WatchTable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("WatchTable").field(&self.0.volume()).finish()
    }
}

/// A wait-set as the registry holds it: its owner, fixed for its life and so
/// read without the set's lock, and the set behind a lock of its own.
struct Held {
    /// Process that created the set, whose teardown releases it. Only its
    /// threads may add/remove members, wait on it, or have it observed; every
    /// entry point owner-checks against it (no ambient authority).
    owner: u64,
    set: SpinLock<WaitSet>,
}

/// A caller-owned wait-set: its growable membership and the round-robin
/// position that keeps the membership fair.
struct WaitSet {
    members: Vec<Member>,
    /// The `(kind, id)` the previous successful wait reported, so the next
    /// scan resumes *after* it.
    ///
    /// A wait reports one member, so without this the scan would always
    /// start at the head and a source that is ready on every scan would
    /// permanently starve every source behind it — a desktop draining a
    /// dragged pointer would never serve the endpoint its applications are
    /// blocked on.
    resume_after: Option<(WaitSourceKind, u64)>,
    /// What the owner is charged for the set itself.
    _set_charge: Charge,
    /// What the owner is charged for the set's members, one unit each.
    member_charge: Charge,
    /// The buffers a wait works in, room for every member reserved as each
    /// is added; out while a wait holds it.
    scratch: Option<WaitScratch>,
    /// Set once the registry has let go of it, so a call that found it just
    /// before is refused as though it had not.
    closed: bool,
}

/// A wait-queue a wait on a set files rows on.
#[derive(Clone, Copy)]
enum Pledged {
    Serve,
    Irq,
    ProcWait,
    SeatInput,
    Stream,
    Console,
    Signal,
    PeerExit,
    Notice,
    Foreground,
    HwTree,
    ConsoleDamage,
    PortRoom,
    Budget,
    Call,
    FsWatch,
}

impl Pledged {
    const ALL: [Self; 16] = [
        Self::Serve,
        Self::Irq,
        Self::ProcWait,
        Self::SeatInput,
        Self::Stream,
        Self::Console,
        Self::Signal,
        Self::PeerExit,
        Self::Notice,
        Self::Foreground,
        Self::HwTree,
        Self::ConsoleDamage,
        Self::PortRoom,
        Self::Budget,
        Self::Call,
        Self::FsWatch,
    ];

    fn queue(self) -> &'static crate::waitq::WaitQueue {
        use crate::waitq as q;
        match self {
            Self::Serve => &q::SERVE_WAITQ,
            Self::Irq => &q::IRQ_WAITQ,
            Self::ProcWait => &q::PROCWAIT_WAITQ,
            Self::SeatInput => &q::SEAT_INPUT_WAITQ,
            Self::Stream => &q::STREAM_WAITQ,
            Self::Console => &q::CONSOLE_WAITQ,
            Self::Signal => &q::SIGNAL_INTAKE_WAITQ,
            Self::PeerExit => &q::PEER_EXIT_WAITQ,
            Self::Notice => &q::NOTICE_WAITQ,
            Self::Foreground => &q::FOREGROUND_WAITQ,
            Self::HwTree => &q::HW_TREE_WAITQ,
            Self::ConsoleDamage => &q::CONSOLE_DAMAGE_WAITQ,
            Self::PortRoom => &q::PORT_ROOM_WAITQ,
            Self::Budget => &q::BUDGET_WAITQ,
            Self::Call => &q::CALL_WAITQ,
            Self::FsWatch => &q::FSWATCH_WAITQ,
        }
    }
}

/// The rows a wait on `members` files on each queue: the wait's own timer
/// row, one row a queue where every member of a kind shares the wait's, and
/// one a member where each files its own.
fn rows_for<'a>(members: impl IntoIterator<Item = &'a Member>) -> [usize; Pledged::ALL.len()] {
    let mut rows = [0usize; Pledged::ALL.len()];
    rows[Pledged::Irq as usize] = 1;
    let mut shared = [false; Pledged::ALL.len()];
    for m in members {
        let (share, own): (&[Pledged], Option<Pledged>) = match m.kind {
            WaitSourceKind::Endpoint | WaitSourceKind::Port => (&[Pledged::Serve], None),
            WaitSourceKind::Irq => (&[], Some(Pledged::Irq)),
            WaitSourceKind::Child => (&[Pledged::ProcWait], None),
            WaitSourceKind::SeatInput => (&[Pledged::SeatInput], None),
            WaitSourceKind::Stream => (
                &[Pledged::Console, Pledged::Foreground],
                Some(Pledged::Stream),
            ),
            WaitSourceKind::StreamRoom => (&[], Some(Pledged::Stream)),
            WaitSourceKind::Signal => (&[Pledged::Signal], None),
            WaitSourceKind::PeerExit => (&[Pledged::PeerExit], None),
            WaitSourceKind::SystemNotice => (&[], Some(Pledged::Notice)),
            WaitSourceKind::Foreground => (&[Pledged::Foreground], None),
            WaitSourceKind::HardwareTree => (&[Pledged::HwTree], None),
            WaitSourceKind::ConsoleDamage => (&[Pledged::ConsoleDamage], None),
            WaitSourceKind::PortRoom => (&[Pledged::PortRoom, Pledged::Budget], None),
            WaitSourceKind::CallReply => (&[Pledged::Call], None),
            WaitSourceKind::File | WaitSourceKind::DirWatch => (&[], Some(Pledged::FsWatch)),
        };
        for &queue in share {
            shared[queue as usize] = true;
        }
        if let Some(queue) = own {
            rows[queue as usize] += 1;
        }
    }
    for (rows, shared) in rows.iter_mut().zip(shared) {
        *rows = (*rows).max(usize::from(shared));
    }
    rows
}

/// Rows pledged on each queue a wait files on ([`WaitQueue::pledge`]),
/// given back when dropped.
///
/// [`WaitQueue::pledge`]: crate::waitq::WaitQueue::pledge
#[derive(Default)]
pub struct Pledges {
    rows: [usize; Pledged::ALL.len()],
}

impl Pledges {
    /// Whether any row is pledged: the set's own scratch holds its pledges,
    /// and the fresh buffers lent to a concurrent wait hold none.
    #[must_use]
    pub fn held(&self) -> bool {
        self.rows.iter().any(|&rows| rows > 0)
    }

    /// Pledge whatever `wanted` asks beyond what is held.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when a queue cannot keep the room; nothing more
    /// is pledged.
    fn reach(&mut self, wanted: &[usize; Pledged::ALL.len()]) -> Result<(), Errno> {
        let before = self.rows;
        for (at, queue) in Pledged::ALL.iter().enumerate() {
            let short = wanted[at].saturating_sub(self.rows[at]);
            if short == 0 {
                continue;
            }
            if queue.queue().pledge(short).is_err() {
                self.trim(&before);
                return Err(Errno::OutOfMemory);
            }
            self.rows[at] += short;
        }
        Ok(())
    }

    /// Give back whatever is held beyond `wanted`.
    fn trim(&mut self, wanted: &[usize; Pledged::ALL.len()]) {
        for (at, queue) in Pledged::ALL.iter().enumerate() {
            let excess = self.rows[at].saturating_sub(wanted[at]);
            if excess > 0 {
                queue.queue().unpledge(excess);
                self.rows[at] -= excess;
            }
        }
    }
}

impl Drop for Pledges {
    fn drop(&mut self) {
        self.trim(&[0; Pledged::ALL.len()]);
    }
}

/// The buffers one wait on a set works in, sized as members are added so the
/// wait itself allocates nothing: a wait that cannot have memory under
/// pressure would return at once rather than park. The set's own buffers also
/// hold the room every wait-queue row the wait files is pledged ([`Pledges`]),
/// so the wait files them all without the heap.
#[derive(Default)]
pub struct WaitScratch {
    /// The rows pledged for a wait over these members.
    pub pledges: Pledges,
    /// The members, rotated to resume after the one last reported.
    pub members: Vec<Member>,
    /// The stream wake keys the members name.
    pub stream_keys: Vec<WakeKey>,
    /// The endpoint each `CallReply` member watches, in member order.
    pub replies: Vec<Option<Arc<CallEndpoint>>>,
    /// The watched-node keys the members name, each once.
    pub watch_keys: Vec<WakeKey>,
}

impl WaitScratch {
    /// Make room for `members` members in every buffer, emptied.
    fn reserve(&mut self, members: usize) -> Result<(), Errno> {
        self.clear();
        self.members
            .try_reserve_exact(members)
            .and(self.stream_keys.try_reserve_exact(members))
            .and(self.replies.try_reserve_exact(members))
            .and(self.watch_keys.try_reserve_exact(members))
            .map_err(|_| Errno::OutOfMemory)
    }

    /// Empty every buffer, letting go of what the last wait held.
    fn clear(&mut self) {
        self.members.clear();
        self.stream_keys.clear();
        self.replies.clear();
        self.watch_keys.clear();
    }
}

/// The lock-guarded registry state.
struct Inner {
    /// Next handle value; monotonic for the life of the kernel so a retired
    /// handle is never reused (a stale handle then resolves to nothing rather
    /// than to a different task's set).
    next_handle: u64,
    /// Fallible to grow, so a refused heap fails the create rather than the
    /// kernel.
    sets: tairix_collections::RadixTree<Arc<Held>>,
    /// Every set's `(owner, handle)`.
    by_owner: tairix_collections::OrderedSet<(u64, u64)>,
}

/// What a thread owes since its wait reported a port's room ready.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OwedRoom {
    /// The port whose slot it holds until it sends there.
    pub port: u64,
    /// The budget release that wait kept, passed on should it not send.
    pub budget: Option<crate::waitq::Chosen>,
}

/// What a thread owes since its wait reported a port's room ready, kept on the
/// thread's own gate, so a call it makes owing nothing learns so in one load.
pub(crate) struct RoomDebt {
    /// Whether `owed` holds a debt. Only the thread records one, and another
    /// party takes one only once the thread has stopped.
    owing: AtomicBool,
    owed: SpinLock<Option<OwedRoom>>,
}

/// A wait asked its thread to owe a second debt while one was recorded, which
/// a wait settles before its thread can owe again.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct AlreadyOwing;

impl RoomDebt {
    /// Nothing owed.
    pub(crate) const fn new() -> Self {
        Self {
            owing: AtomicBool::new(false),
            owed: SpinLock::new(None),
        }
    }

    /// Record `owed`: the thread's wait reported that port's room ready, and
    /// the slot it holds there waits for its send, its next call that is
    /// neither a send nor a wait watching the port, or its end.
    ///
    /// # Errors
    ///
    /// [`AlreadyOwing`] when a debt is recorded already, which is kept: the
    /// caller lets `owed` go.
    pub(crate) fn owe(&self, owed: OwedRoom) -> Result<(), AlreadyOwing> {
        let mut held = self.owed.lock();
        if held.is_some() {
            return Err(AlreadyOwing);
        }
        *held = Some(owed);
        self.owing.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// The port owed a send, if any.
    #[must_use]
    pub(crate) fn port(&self) -> Option<u64> {
        if !self.owing.load(Ordering::Relaxed) {
            return None;
        }
        self.owed.lock().as_ref().map(|owed| owed.port)
    }

    /// Take what is owed, if anything.
    pub(crate) fn take(&self) -> Option<OwedRoom> {
        if !self.owing.load(Ordering::Relaxed) {
            return None;
        }
        let mut held = self.owed.lock();
        self.owing.store(false, Ordering::Relaxed);
        held.take()
    }
}

/// The global wait-set registry. Pure data behind a [`SpinLock`]; see the
/// module docs for why it is global.
static WAIT_SETS: SpinLock<Inner> = SpinLock::new(Inner::new());

/// Mint a fresh, empty wait-set owned by `owner`, returning its handle. It
/// holds `set_charge` for itself and grows `member_charge` by a unit for each
/// member added to it.
///
/// # Errors
///
/// [`Errno::OutOfMemory`] when the heap refuses the registry room for it; the
/// charges are returned with the set that was never made.
pub fn create(owner: u64, set_charge: Charge, member_charge: Charge) -> Result<u64, Errno> {
    WAIT_SETS.lock().create(owner, set_charge, member_charge)
}

impl Inner {
    const fn new() -> Self {
        Self {
            // Start above zero so a caller-zeroed handle slot is never a live
            // set.
            next_handle: 1,
            sets: tairix_collections::RadixTree::new(),
            by_owner: tairix_collections::OrderedSet::new(),
        }
    }

    /// [`create`] against this registry.
    fn create(
        &mut self,
        owner: u64,
        set_charge: Charge,
        member_charge: Charge,
    ) -> Result<u64, Errno> {
        let handle = self.next_handle;
        let mut scratch = WaitScratch::default();
        scratch.pledges.reach(&rows_for([]))?;
        let held = Arc::try_new(Held {
            owner,
            set: SpinLock::new(WaitSet {
                members: Vec::new(),
                resume_after: None,
                _set_charge: set_charge,
                member_charge,
                scratch: Some(scratch),
                closed: false,
            }),
        })
        .map_err(|_| Errno::OutOfMemory)?;
        self.by_owner
            .try_insert((owner, handle))
            .map_err(|_| Errno::OutOfMemory)?;
        if self.sets.try_insert(handle, held).is_err() {
            self.by_owner.remove(&(owner, handle));
            return Err(Errno::OutOfMemory);
        }
        self.next_handle = self.next_handle.wrapping_add(1);
        Ok(handle)
    }

    /// Let go of the set `handle` if `owner` owns it.
    fn take(&mut self, owner: u64, handle: u64) -> Option<Arc<Held>> {
        if !self.by_owner.remove(&(owner, handle)) {
            return None;
        }
        self.sets.remove(handle)
    }
}

/// Resolve `handle` to a set owned by `owner`, applying the owner check, and
/// run `f` against its mutable [`WaitSet`].
///
/// Returns [`Errno::NotFound`] if the handle names no set or names one owned by
/// another task — a forged or foreign handle resolves to nothing (identify
/// before acting, fail closed).
fn with_owned<R>(owner: u64, handle: u64, f: impl FnOnce(&mut WaitSet) -> R) -> Result<R, Errno> {
    lookup(owner, handle)?.with(f)
}

/// The set `handle` if `owner` owns it, held so the registry's lock is down
/// before the set's own is taken.
fn lookup(owner: u64, handle: u64) -> Result<Arc<Held>, Errno> {
    WAIT_SETS
        .lock()
        .sets
        .get(handle)
        .filter(|held| held.owner == owner)
        .map(Arc::clone)
        .ok_or(Errno::NotFound)
}

impl Held {
    /// Run `f` against the set, unless it has closed since it was found.
    fn with<R>(&self, f: impl FnOnce(&mut WaitSet) -> R) -> Result<R, Errno> {
        let mut set = self.set.lock();
        if set.closed {
            return Err(Errno::NotFound);
        }
        Ok(f(&mut set))
    }
}

/// Add `member` to the owner's wait-set `handle`.
///
/// The *resource* the member names is resolved and owner-checked by the
/// syscall handler before this is called; here only the set is owner-checked.
///
/// # Errors
///
/// * [`Errno::NotFound`] if `handle` is not a wait-set owned by `owner`.
/// * [`Errno::AlreadyExists`] if a member with the same `(kind, id)` is already
///   registered (an unambiguous duplicate, mirroring `epoll_ctl(ADD)` of an
///   already-registered source — fail closed rather than silently double-add).
/// * [`Errno::LimitExceeded`] if one more member would carry the owner past
///   `member_limit`, its `wait-set-members` limit.
/// * [`Errno::OutOfMemory`] if the set cannot grow to hold it; nothing is
///   added or charged.
pub fn add(owner: u64, handle: u64, member: Member, member_limit: u64) -> Result<(), Errno> {
    with_owned(owner, handle, |set| {
        if set
            .members
            .iter()
            .any(|m| m.kind == member.kind && m.id == member.id)
        {
            return Err(Errno::AlreadyExists);
        }
        set.members.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        let members = set.members.len() + 1;
        let wanted = rows_for(set.members.iter().chain(core::iter::once(&member)));
        // A wait under way holds the buffers and their pledges: the next one
        // gets its own now, and the lent one is dropped when it comes back.
        let scratch = set.scratch.get_or_insert_with(WaitScratch::default);
        scratch.reserve(members)?;
        set.member_charge.grow(1, member_limit)?;
        if let Err(refused) = scratch.pledges.reach(&wanted) {
            set.member_charge.shrink(1);
            return Err(refused);
        }
        set.members.push(member);
        Ok(())
    })?
}

/// Remove the member with `(kind, id)` from the owner's wait-set `handle`.
///
/// # Errors
///
/// * [`Errno::NotFound`] if `handle` is not a wait-set owned by `owner`, or if
///   no member with that `(kind, id)` is registered.
pub fn remove(owner: u64, handle: u64, kind: WaitSourceKind, id: u64) -> Result<(), Errno> {
    // Collect the removed File member's identity under the set lock, then
    // drop its file-change watch after releasing the lock (so the fswatch
    // table lock is never taken nested under this one).
    let removed = with_owned(owner, handle, |set| {
        let at = set
            .members
            .iter()
            .position(|m| m.kind == kind && m.id == id)
            .ok_or(Errno::NotFound)?;
        set.member_charge.shrink(1);
        let removed = set.members.remove(at);
        let wanted = rows_for(&set.members);
        if let Some(scratch) = &mut set.scratch {
            scratch.pledges.trim(&wanted);
        }
        Ok::<_, Errno>(removed)
    })??;
    release_file_member(&removed);
    Ok(())
}

/// Release the node registration a File member holds, after the set lock is
/// dropped so the watch table's lock never nests under it.
fn release_file_member(member: &Member) {
    if member.kind == WaitSourceKind::File {
        if let Some(table) = &member.table {
            table.0.file_member_remove(member.node);
        }
    }
}

/// Record the edge an edge-triggered member last reported (the consume
/// performed when the member is reported ready). A no-op if the member is
/// absent — the owner may have removed it between the readiness scan and this
/// update.
///
/// # Errors
///
/// [`Errno::NotFound`] if `handle` is not a wait-set owned by `owner`.
pub fn advance_observed(
    owner: u64,
    handle: u64,
    kind: WaitSourceKind,
    id: u64,
    observed: u64,
) -> Result<(), Errno> {
    with_owned(owner, handle, |set| {
        if let Some(member) = find(set, kind, id) {
            member.observed = observed;
        }
    })
}

/// Record what a `DirWatch` member just reported: the journal position and
/// mount-table epoch it covered, and when, which starts its latency.
///
/// # Errors
///
/// [`Errno::NotFound`] if `handle` is not a wait-set owned by `owner`.
pub fn advance_paced(
    owner: u64,
    handle: u64,
    id: u64,
    observed: u64,
    mounts: u64,
    now: u64,
) -> Result<(), Errno> {
    with_owned(owner, handle, |set| {
        if let Some(member) = find(set, WaitSourceKind::DirWatch, id) {
            member.pacing.observed = observed;
            member.pacing.mounts = mounts;
            member.pacing.reported_at = Some(now);
        }
    })
}

fn find(set: &mut WaitSet, kind: WaitSourceKind, id: u64) -> Option<&mut Member> {
    set.members
        .iter_mut()
        .find(|m| m.kind == kind && m.id == id)
}

/// Lend a wait on the owner's set `handle` its scratch, the members in it
/// rotated to begin just after the member [`note_reported`] last recorded, so
/// successive waits hand the ready members out round-robin rather than always
/// awarding the first-registered one. Registration order is kept within a
/// rotation, and a set whose last-reported member has since gone falls back to
/// it. The wait scans without this registry's lock held, and gives the scratch
/// back with [`checkin`]; a second concurrent wait on the set is lent fresh
/// buffers.
///
/// # Errors
///
/// [`Errno::NotFound`] if `handle` is not a wait-set owned by `owner`;
/// [`Errno::OutOfMemory`] if fresh buffers cannot be had.
pub fn checkout(owner: u64, handle: u64) -> Result<WaitScratch, Errno> {
    with_owned(owner, handle, |set| {
        let resume = set
            .resume_after
            .and_then(|(kind, id)| {
                set.members
                    .iter()
                    .position(|m| m.kind == kind && m.id == id)
            })
            .map_or(0, |at| at + 1);
        let mut scratch = set.scratch.take().unwrap_or_default();
        if let Err(refused) = scratch.reserve(set.members.len()) {
            set.scratch = Some(scratch);
            return Err(refused);
        }
        scratch.members.extend_from_slice(&set.members[resume..]);
        scratch.members.extend_from_slice(&set.members[..resume]);
        Ok(scratch)
    })?
}

/// Take back the scratch [`checkout`] lent a wait on the owner's set `handle`,
/// emptied and its pledges matched to the members; one the set no longer
/// needs, or a set that has gone, drops it. The set keeps buffers holding
/// pledges over buffers holding none.
pub fn checkin(owner: u64, handle: u64, mut scratch: WaitScratch) {
    scratch.clear();
    let _ = with_owned(owner, handle, move |set| {
        let keep = set
            .scratch
            .as_ref()
            .is_none_or(|home| !home.pledges.held() && scratch.pledges.held());
        if keep {
            let wanted = rows_for(&set.members);
            scratch.pledges.trim(&wanted);
            // Buffers lent without pledges take them where the heap allows;
            // where it does not, the pledged buffers replace them on return.
            let _ = scratch.pledges.reach(&wanted);
            set.scratch = Some(scratch);
        }
    });
}

/// Record `(kind, id)` as the member the wait just reported, so the next
/// [`checkout`] snapshot resumes after it.
///
/// # Errors
///
/// [`Errno::NotFound`] if `handle` is not a wait-set owned by `owner`.
pub fn note_reported(owner: u64, handle: u64, kind: WaitSourceKind, id: u64) -> Result<(), Errno> {
    with_owned(owner, handle, |set| {
        set.resume_after = Some((kind, id));
    })
}

/// Close the owner's wait-set `handle`, releasing its members and returning
/// what it was charged.
///
/// A wait another thread of the owner has under way on the set ends at its
/// next event or deadline, as an `epoll_wait` on a descriptor closed under it
/// does.
///
/// # Errors
///
/// [`Errno::NotFound`] if `handle` is not a wait-set owned by `owner`.
pub fn close(owner: u64, handle: u64) -> Result<(), Errno> {
    let held = WAIT_SETS
        .lock()
        .take(owner, handle)
        .ok_or(Errno::NotFound)?;
    retire(&held);
    Ok(())
}

/// Tear down every wait-set owned by the exiting task `owner`, returning how
/// many were removed.
///
/// A wait-set holds no resource of its own (its members only *name* endpoints
/// and IRQ lines the task owns, which are reclaimed by their own teardown), so
/// dropping the sets is the whole reclamation. Idempotent.
pub fn release_owned_by(owner: u64) -> usize {
    // One set at a time, so a teardown allocates nothing; each set's file
    // watches are released after the lock is dropped.
    let mut removed = 0;
    loop {
        let held = {
            let mut g = WAIT_SETS.lock();
            let next = g
                .by_owner
                .range((owner, 0)..=(owner, u64::MAX))
                .next()
                .copied();
            next.and_then(|(_, handle)| g.take(owner, handle))
        };
        let Some(held) = held else {
            return removed;
        };
        removed += 1;
        retire(&held);
    }
}

/// End a set the registry has let go of: closed under its own lock, so a call
/// that found it just before is refused, and its members' watches released
/// once that lock is down.
fn retire(held: &Held) {
    let members = {
        let mut set = held.set.lock();
        set.closed = true;
        set.scratch = None;
        core::mem::take(&mut set.members)
    };
    for member in &members {
        release_file_member(member);
    }
}

/// `true` if `handle` is a live wait-set owned by `owner`. Diagnostic / test
/// observer.
#[must_use]
pub fn owned_by(owner: u64, handle: u64) -> bool {
    WAIT_SETS.lock().by_owner.get(&(owner, handle)).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The members a wait would scan, in its order.
    fn members(owner: u64, handle: u64) -> Result<Vec<Member>, Errno> {
        let scratch = checkout(owner, handle)?;
        let members = scratch.members.clone();
        checkin(owner, handle, scratch);
        Ok(members)
    }
    use alloc::sync::Arc;
    use tairix_abi::{LimitKind, RLIMIT_INFINITY};
    use tairix_kernel_sec::ResourceCharges;

    /// A set charged to counters no other test shares.
    fn new_set(owner: u64) -> u64 {
        set_charged_to(owner, &Arc::new(ResourceCharges::new()))
    }

    fn set_charged_to(owner: u64, charges: &Arc<ResourceCharges>) -> u64 {
        let set = charges
            .charge(LimitKind::WaitSets, 1, RLIMIT_INFINITY)
            .expect("unlimited");
        create(owner, set, charges.empty(LimitKind::WaitSetMembers)).expect("room")
    }

    /// A thread owes one debt at a time: a second is refused as the breach it
    /// is and the first kept, and what is owed is taken once.
    ///
    /// Regression: a second record replaced the first, stranding the slot the
    /// first named and losing the budget release it kept.
    #[test]
    fn a_thread_owes_one_debt_at_a_time_and_a_second_is_refused() {
        let debt = RoomDebt::new();
        let at = |port| OwedRoom { port, budget: None };
        assert_eq!(debt.port(), None, "nothing owed");
        assert_eq!(debt.owe(at(0x10)), Ok(()));
        assert_eq!(debt.owe(at(0x11)), Err(AlreadyOwing));
        assert_eq!(debt.port(), Some(0x10), "the first is kept");
        assert_eq!(debt.take(), Some(at(0x10)));
        assert_eq!(debt.take(), None, "taken once");
        assert_eq!(debt.owe(at(0x12)), Ok(()), "settled, it may owe again");
        assert_eq!(debt.take(), Some(at(0x12)));
    }

    /// A member the heap has no room for is refused for memory, neither added
    /// nor charged; given room, it is added.
    #[test]
    fn a_member_the_heap_refuses_is_neither_added_nor_charged() {
        let charges = Arc::new(ResourceCharges::new());
        let h = set_charged_to(0xD472, &charges);
        let refused = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    crate::test_alloc::refuse_allocations_on_current_thread(true);
                    let added = put(0xD472, h, ep(7, 70));
                    crate::test_alloc::refuse_allocations_on_current_thread(false);
                    added
                })
                .join()
                .expect("the adding thread ends")
        });
        assert_eq!(refused, Err(Errno::OutOfMemory));
        assert_eq!(
            charges.held(LimitKind::WaitSetMembers),
            0,
            "nothing charged"
        );
        assert_eq!(members(0xD472, h).map(|m| m.len()), Ok(0));
        assert_eq!(put(0xD472, h, ep(7, 70)), Ok(()));
        assert_eq!(release_owned_by(0xD472), 1);
    }

    /// A wait works in buffers reserved as its members were added, so a heap
    /// that refuses everything still lends it its snapshot; only a second
    /// wait while the first holds them needs buffers of its own, and is
    /// refused for memory rather than aborting the kernel.
    #[test]
    fn a_wait_needs_no_memory_once_its_members_are_added() {
        let h = new_set(0xD473);
        assert_eq!(put(0xD473, h, ep(7, 70)), Ok(()));
        let (lent, second) = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    crate::test_alloc::refuse_allocations_on_current_thread(true);
                    let first = checkout(0xD473, h);
                    let second = checkout(0xD473, h).map(|scratch| scratch.members.len());
                    let lent = first.map(|scratch| {
                        let members = scratch.members.len();
                        checkin(0xD473, h, scratch);
                        members
                    });
                    crate::test_alloc::refuse_allocations_on_current_thread(false);
                    (lent, second)
                })
                .join()
                .expect("the waiting thread ends")
        });
        assert_eq!(lent, Ok(1));
        assert_eq!(second, Err(Errno::OutOfMemory));
        assert_eq!(members(0xD473, h).map(|m| m.len()), Ok(1));
        assert_eq!(release_owned_by(0xD473), 1);
    }

    /// A member added while a wait holds the set's buffers gets room of its own
    /// for the next wait, which then needs no memory; the buffers the first
    /// wait hands back are dropped rather than kept too small.
    ///
    /// Regression: such an add reserved nothing, so the next wait found its
    /// buffers short and asked the heap for them.
    #[test]
    fn a_member_added_during_a_wait_is_room_for_the_next() {
        let h = new_set(0xD474);
        assert_eq!(put(0xD474, h, ep(7, 70)), Ok(()));
        let lent = checkout(0xD474, h).expect("the first wait's buffers");
        assert_eq!(put(0xD474, h, ep(8, 80)), Ok(()));
        checkin(0xD474, h, lent);
        let next = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    crate::test_alloc::refuse_allocations_on_current_thread(true);
                    let next = checkout(0xD474, h).map(|scratch| {
                        let members = scratch.members.len();
                        checkin(0xD474, h, scratch);
                        members
                    });
                    crate::test_alloc::refuse_allocations_on_current_thread(false);
                    next
                })
                .join()
                .expect("the waiting thread ends")
        });
        assert_eq!(next, Ok(2));
        assert_eq!(release_owned_by(0xD474), 1);
    }

    /// A registry the heap refuses room in fails the create closed, the
    /// charges returned with the set that was never made, rather than
    /// aborting the kernel, and its handle is not spent.
    #[test]
    fn a_create_the_heap_refuses_fails_closed_and_returns_its_charges() {
        let charges = Arc::new(ResourceCharges::new());
        let mut registry = Inner::new();
        let refused = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let set = charges
                        .charge(LimitKind::WaitSets, 1, RLIMIT_INFINITY)
                        .expect("unlimited");
                    let members = charges.empty(LimitKind::WaitSetMembers);
                    crate::test_alloc::refuse_allocations_on_current_thread(true);
                    let made = registry.create(0xD471, set, members);
                    crate::test_alloc::refuse_allocations_on_current_thread(false);
                    made
                })
                .join()
                .expect("the creating thread ends")
        });
        assert_eq!(refused, Err(Errno::OutOfMemory));
        assert_eq!(charges.held(LimitKind::WaitSets), 0, "its charge returned");
        assert_eq!(registry.next_handle, 1, "no handle spent");
        assert!(
            registry.by_owner.get(&(0xD471, 1)).is_none(),
            "no owner entry left for the set never made"
        );
        let set = charges
            .charge(LimitKind::WaitSets, 1, RLIMIT_INFINITY)
            .expect("unlimited");
        let members = charges.empty(LimitKind::WaitSetMembers);
        assert_eq!(registry.create(0xD471, set, members), Ok(1));
    }

    fn put(owner: u64, handle: u64, member: Member) -> Result<(), Errno> {
        add(owner, handle, member, RLIMIT_INFINITY)
    }

    /// A call that found a set just before it closed is refused once the set
    /// has closed, so nothing it would have added outlives the set's teardown.
    #[test]
    fn a_set_found_before_it_closed_refuses_the_call_that_found_it() {
        let owner = 0xD475;
        let h = new_set(owner);
        put(owner, h, ep(1, 10)).expect("added");
        let found = lookup(owner, h).expect("found");
        close(owner, h).expect("closed");
        assert_eq!(
            found.with(|set| set.members.push(ep(2, 20))),
            Err(Errno::NotFound)
        );
        let set = found.set.lock();
        assert!(set.members.is_empty(), "the teardown took every member");
        assert!(set.scratch.is_none(), "and let go of the wait's buffers");
    }

    fn ep(id: u64, token: u64) -> Member {
        of(WaitSourceKind::Endpoint, id, token)
    }

    /// A wait files the timer row always, one row a queue its members share,
    /// and one a member where each files its own.
    #[test]
    fn a_wait_files_the_rows_its_members_name() {
        let rows = |members: &[Member]| rows_for(members);
        let mut empty = [0; Pledged::ALL.len()];
        empty[Pledged::Irq as usize] = 1;
        assert_eq!(rows(&[]), empty, "a sleep files its timer");
        let set = [
            ep(1, 0),
            of(WaitSourceKind::Port, 2, 0),
            of(WaitSourceKind::Irq, 3, 0),
            of(WaitSourceKind::Irq, 4, 0),
            of(WaitSourceKind::Stream, 5, 0),
            of(WaitSourceKind::StreamRoom, 6, 0),
            of(WaitSourceKind::SystemNotice, 7, 0),
            of(WaitSourceKind::PortRoom, 8, 0),
            of(WaitSourceKind::PortRoom, 9, 0),
            of(WaitSourceKind::File, 10, 0),
            of(WaitSourceKind::DirWatch, 11, 0),
        ];
        let filed = rows(&set);
        let at = |queue: Pledged| filed[queue as usize];
        assert_eq!(at(Pledged::Serve), 1, "endpoints and ports share one row");
        assert_eq!(at(Pledged::Irq), 3, "the timer and a row a line");
        assert_eq!(at(Pledged::Stream), 2);
        assert_eq!((at(Pledged::Console), at(Pledged::Foreground)), (1, 1));
        assert_eq!(at(Pledged::Notice), 1);
        assert_eq!((at(Pledged::PortRoom), at(Pledged::Budget)), (1, 1));
        assert_eq!(at(Pledged::FsWatch), 2);
        assert_eq!(at(Pledged::Call), 0);
    }

    /// A set's own buffers hold exactly the pledges its members want, from its
    /// creation through every add and remove; buffers lent to a concurrent
    /// wait hold none, and the set's own come back trimmed to what is left.
    #[test]
    fn a_sets_pledges_follow_its_members() {
        let owner = crate::test_boot::claim_task();
        let handle = new_set(owner);
        let pledged = |owner, handle| {
            lookup(owner, handle)
                .expect("found")
                .set
                .lock()
                .scratch
                .as_ref()
                .map(|scratch| scratch.pledges.rows)
        };
        assert_eq!(pledged(owner, handle), Some(rows_for([])));
        let stream = of(WaitSourceKind::Stream, 5, 0);
        let notice = of(WaitSourceKind::SystemNotice, 7, 0);
        add(owner, handle, stream.clone(), u64::MAX).expect("added");
        add(owner, handle, notice.clone(), u64::MAX).expect("added");
        assert_eq!(pledged(owner, handle), Some(rows_for([&stream, &notice])));
        let lent = checkout(owner, handle).expect("lent");
        assert!(lent.pledges.held());
        let concurrent = checkout(owner, handle).expect("lent");
        assert!(
            !concurrent.pledges.held(),
            "a concurrent wait files unpledged"
        );
        remove(owner, handle, WaitSourceKind::Stream, 5).expect("removed");
        checkin(owner, handle, concurrent);
        assert_eq!(
            pledged(owner, handle),
            Some(rows_for([&notice])),
            "buffers that came back unpledged take the pledges"
        );
        checkin(owner, handle, lent);
        assert_eq!(pledged(owner, handle), Some(rows_for([&notice])));
        close(owner, handle).expect("closed");
    }

    fn of(kind: WaitSourceKind, id: u64, token: u64) -> Member {
        Member {
            kind,
            id,
            token,
            node: 0,
            observed: 0,
            pacing: crate::fswatch::Pacing::default(),
            table: None,
            pin: None,
        }
    }

    /// Only a wait holding an endpoint or a port hears requests, so a post
    /// wakes no wait on a stream, a line, a child or a notice.
    ///
    /// Regression: every wait joined the queue posts wake, so each request to
    /// any server unparked every wait-set waiter on the machine.
    #[test]
    fn only_an_endpoint_or_port_member_hears_requests() {
        let quiet = [
            of(WaitSourceKind::Stream, 3, 1),
            of(WaitSourceKind::Irq, 4, 2),
            of(WaitSourceKind::Child, 0, 3),
            of(WaitSourceKind::SystemNotice, 1, 4),
        ];
        assert!(!hears_requests(&quiet));
        assert!(!hears_requests(&[]));
        for kind in [WaitSourceKind::Endpoint, WaitSourceKind::Port] {
            let mut members = quiet.to_vec();
            members.push(of(kind, 9, 5));
            assert!(hears_requests(&members), "{kind:?}");
        }
    }

    #[test]
    fn create_then_owner_checked_access() {
        let h = new_set(0x1001);
        assert!(owned_by(0x1001, h));
        // A different task cannot see or mutate the set.
        assert!(!owned_by(0x2002, h));
        assert_eq!(members(0x2002, h), Err(Errno::NotFound));
        assert_eq!(
            put(0x2002, h, ep(7, 70)),
            Err(Errno::NotFound),
            "foreign add is refused"
        );
        // Clean up so the global registry does not leak across tests.
        assert_eq!(release_owned_by(0x1001), 1);
    }

    #[test]
    fn add_remove_and_duplicate_is_refused() {
        let h = new_set(0x3003);
        assert_eq!(put(0x3003, h, ep(7, 70)), Ok(()));
        assert_eq!(put(0x3003, h, ep(8, 80)), Ok(()));
        // Same (kind,id) is a duplicate, even with a different token.
        assert_eq!(put(0x3003, h, ep(7, 99)), Err(Errno::AlreadyExists));
        assert_eq!(members(0x3003, h).map(|m| m.len()), Ok(2));
        assert_eq!(remove(0x3003, h, WaitSourceKind::Endpoint, 7), Ok(()));
        assert_eq!(members(0x3003, h).map(|m| m.len()), Ok(1));
        // Removing an absent member fails closed.
        assert_eq!(
            remove(0x3003, h, WaitSourceKind::Endpoint, 7),
            Err(Errno::NotFound)
        );
        assert_eq!(release_owned_by(0x3003), 1);
    }

    #[test]
    fn an_endpoint_and_an_irq_of_the_same_id_are_distinct_members() {
        let h = new_set(0x4004);
        assert_eq!(put(0x4004, h, ep(5, 50)), Ok(()));
        assert_eq!(
            put(
                0x4004,
                h,
                Member {
                    kind: WaitSourceKind::Irq,
                    id: 5,
                    token: 55,
                    node: 0,
                    observed: 0,
                    pacing: crate::fswatch::Pacing::default(),
                    table: None,
                    pin: None,
                }
            ),
            Ok(()),
            "same id, different kind is not a duplicate"
        );
        assert_eq!(members(0x4004, h).map(|m| m.len()), Ok(2));
        assert_eq!(release_owned_by(0x4004), 1);
    }

    #[test]
    fn release_owned_by_drops_only_the_owner_and_is_idempotent() {
        let a = new_set(0x5005);
        let b = new_set(0x6006);
        assert_eq!(release_owned_by(0x5005), 1);
        assert!(!owned_by(0x5005, a));
        assert!(owned_by(0x6006, b));
        assert_eq!(release_owned_by(0x5005), 0, "idempotent");
        assert_eq!(release_owned_by(0x6006), 1);
    }

    #[test]
    fn handles_are_unique_across_creates() {
        let a = new_set(0x7007);
        let b = new_set(0x7007);
        assert_ne!(a, b, "each create mints a fresh handle");
        assert_eq!(release_owned_by(0x7007), 2);
    }

    fn ids(owner: u64, handle: u64) -> Vec<u64> {
        members(owner, handle)
            .expect("owned set")
            .iter()
            .map(|m| m.id)
            .collect()
    }

    #[test]
    fn a_fresh_set_scans_in_registration_order() {
        let h = new_set(0x8008);
        for id in 1..=3 {
            assert_eq!(put(0x8008, h, ep(id, id * 10)), Ok(()));
        }
        assert_eq!(ids(0x8008, h), alloc::vec![1, 2, 3]);
        assert_eq!(release_owned_by(0x8008), 1);
    }

    /// The regression this cursor exists for: the scan takes the first ready
    /// member, so a source ready on every scan would hold the head forever
    /// and never let the members behind it be reported. Each report moves the
    /// start on, so every member reaches the head within one lap.
    #[test]
    fn reporting_a_member_moves_the_scan_past_it() {
        let h = new_set(0x9009);
        for id in 1..=3 {
            assert_eq!(put(0x9009, h, ep(id, id * 10)), Ok(()));
        }
        assert_eq!(
            note_reported(0x9009, h, WaitSourceKind::Endpoint, 1),
            Ok(())
        );
        assert_eq!(ids(0x9009, h), alloc::vec![2, 3, 1]);
        assert_eq!(
            note_reported(0x9009, h, WaitSourceKind::Endpoint, 2),
            Ok(())
        );
        assert_eq!(ids(0x9009, h), alloc::vec![3, 1, 2]);
        // A lap brings the first member back to the head, so the rotation
        // cycles rather than walking off the end.
        assert_eq!(
            note_reported(0x9009, h, WaitSourceKind::Endpoint, 3),
            Ok(())
        );
        assert_eq!(ids(0x9009, h), alloc::vec![1, 2, 3]);
        assert_eq!(release_owned_by(0x9009), 1);
    }

    #[test]
    fn the_rotation_is_per_kind_as_well_as_per_id() {
        let h = new_set(0xa00a);
        assert_eq!(put(0xa00a, h, ep(5, 50)), Ok(()));
        assert_eq!(
            put(
                0xa00a,
                h,
                Member {
                    kind: WaitSourceKind::Irq,
                    id: 5,
                    token: 55,
                    node: 0,
                    observed: 0,
                    pacing: crate::fswatch::Pacing::default(),
                    table: None,
                    pin: None,
                }
            ),
            Ok(())
        );
        // Reporting the endpoint must not move past the same-id IRQ member.
        assert_eq!(
            note_reported(0xa00a, h, WaitSourceKind::Endpoint, 5),
            Ok(())
        );
        assert_eq!(
            members(0xa00a, h).expect("owned set")[0].kind,
            WaitSourceKind::Irq
        );
        assert_eq!(release_owned_by(0xa00a), 1);
    }

    #[test]
    fn removing_the_last_reported_member_falls_back_to_registration_order() {
        let h = new_set(0xb00b);
        for id in 1..=3 {
            assert_eq!(put(0xb00b, h, ep(id, id * 10)), Ok(()));
        }
        assert_eq!(
            note_reported(0xb00b, h, WaitSourceKind::Endpoint, 2),
            Ok(())
        );
        assert_eq!(ids(0xb00b, h), alloc::vec![3, 1, 2]);
        assert_eq!(remove(0xb00b, h, WaitSourceKind::Endpoint, 2), Ok(()));
        assert_eq!(ids(0xb00b, h), alloc::vec![1, 3]);
        assert_eq!(release_owned_by(0xb00b), 1);
    }

    #[test]
    fn an_empty_set_rotates_to_nothing() {
        let h = new_set(0xc00c);
        assert_eq!(ids(0xc00c, h), Vec::<u64>::new());
        assert_eq!(
            note_reported(0xc00c, h, WaitSourceKind::Endpoint, 1),
            Ok(()),
            "recording a member the set no longer holds is not an error"
        );
        assert_eq!(ids(0xc00c, h), Vec::<u64>::new());
        assert_eq!(release_owned_by(0xc00c), 1);
    }

    #[test]
    fn note_reported_is_owner_checked() {
        let h = new_set(0xd00d);
        assert_eq!(
            note_reported(0xe00e, h, WaitSourceKind::Endpoint, 1),
            Err(Errno::NotFound)
        );
        assert_eq!(release_owned_by(0xd00d), 1);
    }

    #[test]
    fn a_set_and_its_members_are_charged_until_the_set_goes() {
        let charges = Arc::new(ResourceCharges::new());
        let h = set_charged_to(0xe11e, &charges);
        assert_eq!(charges.held(LimitKind::WaitSets), 1);
        assert_eq!(add(0xe11e, h, ep(1, 10), 2), Ok(()));
        assert_eq!(add(0xe11e, h, ep(2, 20), 2), Ok(()));
        assert_eq!(
            add(0xe11e, h, ep(3, 30), 2),
            Err(Errno::LimitExceeded),
            "a member past the limit is refused"
        );
        assert_eq!(ids(0xe11e, h), alloc::vec![1, 2], "and is not recorded");
        assert_eq!(
            add(0xe11e, h, ep(1, 99), 2),
            Err(Errno::AlreadyExists),
            "a duplicate is a duplicate before it is a charge"
        );
        assert_eq!(remove(0xe11e, h, WaitSourceKind::Endpoint, 1), Ok(()));
        assert_eq!(charges.held(LimitKind::WaitSetMembers), 1);
        assert_eq!(add(0xe11e, h, ep(3, 30), 2), Ok(()), "a removal makes room");
        assert_eq!(release_owned_by(0xe11e), 1);
        assert_eq!(charges.held(LimitKind::WaitSets), 0);
        assert_eq!(charges.held(LimitKind::WaitSetMembers), 0);
    }

    #[test]
    fn close_releases_one_owned_set_and_refuses_any_other() {
        let charges = Arc::new(ResourceCharges::new());
        let kept = set_charged_to(0xf11f, &charges);
        let closed = set_charged_to(0xf11f, &charges);
        assert_eq!(put(0xf11f, closed, ep(1, 10)), Ok(()));
        assert_eq!(
            close(0x0bad, closed),
            Err(Errno::NotFound),
            "a foreign close"
        );
        assert_eq!(close(0xf11f, closed), Ok(()));
        assert!(!owned_by(0xf11f, closed));
        assert_eq!(
            close(0xf11f, closed),
            Err(Errno::NotFound),
            "a second close"
        );
        assert_eq!(members(0xf11f, closed), Err(Errno::NotFound));
        assert_eq!(charges.held(LimitKind::WaitSets), 1);
        assert_eq!(charges.held(LimitKind::WaitSetMembers), 0);
        assert!(owned_by(0xf11f, kept), "the owner's other set is untouched");
        assert_eq!(release_owned_by(0xf11f), 1);
    }

    #[test]
    fn teardown_removes_every_set_of_the_owner_among_others() {
        let mine: Vec<u64> = (0..4)
            .flat_map(|_| [new_set(0x1221), new_set(0x1331)])
            .collect();
        assert_eq!(release_owned_by(0x1221), 4);
        for handle in &mine {
            assert!(!owned_by(0x1221, *handle));
        }
        assert_eq!(release_owned_by(0x1221), 0, "idempotent");
        assert_eq!(
            release_owned_by(0x1331),
            4,
            "interleaved sets are another owner's"
        );
    }
}
