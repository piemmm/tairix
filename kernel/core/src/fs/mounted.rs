//! The production [`FilesystemService`]: the `fs_*` syscalls served against
//! a mounted volume.
//!
//! The hollow [`NULL_FILESYSTEM`](super::service::NULL_FILESYSTEM) default
//! fails every `fs_*` syscall closed. This module is the real producer the
//! boot path installs once the disk is mounted: [`MountedFilesystemService`]
//! resolves the caller's **kernel-attested** identity into full VFS
//! [`Credentials`] and authorises every operation through the secured VFS.
//!
//! # Concurrent, caller-context, per-mount-serialised
//!
//! Each `fs_*` operation runs in the **calling task's own context**, directly
//! against the resolved mount, so N tasks drive N concurrent operations and a
//! task waiting on a slow device completion parks on *its own* block-driver
//! IRQ wait rather than behind a single global server. Operations on
//! *different* mounts proceed fully in parallel. Within one mount the
//! filesystem driver needs `&mut self` per operation and may **park** across a
//! block-device completion IRQ ([`tairix_abi::driver::block::Block::read_blocks`]
//! parks the caller), so the per-mount lock is a scheduler-blocking
//! [`SleepLock`](crate::sleeplock::SleepLock) held across that park — never a `lib/sync` spin lock, which a
//! second contender would busy-spin on while the holder sleeps
//! (`docs/src/architecture/sync.md`). This is the architecture a future
//! async/multi-queue `Block` overlaps operations *within* a device on, with no
//! change above the driver.
//!
//! # Identity is kernel-attested, never caller-supplied
//!
//! The syscall handler supplies an owning `uid` and effective capability set,
//! both read from a task's [`tairix_kernel_sec::TaskCapabilities`] — never
//! anything the caller passed. This service resolves that uid's primary and
//! supplementary **groups** from the authoritative [`IdentityTable`] (a
//! frozen, credential-free index — it carries no password material), then
//! resolves every path through the VFS's one resolver, judging each node by
//! its own record, so every per-inode owner/mode/ACL/`required_cap` and
//! mount-flag check stays kernel-side and fails closed. A principal with no
//! account, or a call made before the identity table or the mount is
//! installed, is denied rather than served.
//!
//! # Descriptors hold pinned nodes
//!
//! A walk holds one projection's driver at a time, and an operation acts on
//! what it reached under the hold of the driver it ended on: an open creates
//! what it was asked to and pins the node, a removal unlinks the name. A
//! rename or link pins both directories it names, then changes them under
//! one hold. A byte operation runs on a pin under the driver's lock, after
//! checking that the node's projection is still in the mount table and its
//! registration is current. A driver's lock may be held while the mount
//! table's guard or the registry's lock is taken, never the other way round:
//! what either holds is copied out before the driver is locked.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tairix_abi::driver::filesystem::{
    DirVisit, FilesystemAttrsFs, FilesystemAttrsProvider, FilesystemRead, FilesystemSecurity,
    FilesystemStats, FilesystemWrite, NodeId, NodeInfo, NodeKind as DriverNodeKind, NodeTimes,
    VolumeHost, VolumeStats,
};
use tairix_abi::driver::DriverHandle;
use tairix_abi::fs::{Access as Wanted, NodeExpect};
use tairix_abi::sysinfo::{
    MountAvailability, MountRecord, MountVolumeState, ViewBindingRecord, VolumeIoHealthRecord,
    VolumeIoQueueRecord, VolumeIoStatsRecord,
};
use tairix_abi::time::{is_plausible_wall_time, Time64};
use tairix_abi::{
    CapabilityQuery, DirEntry, Errno, FileId, FileKind, FileStat, OpenFlags, RealpathMode,
    RenameFlags, SeekRegion, UnlinkFlags, FS_MODE_MASK,
};
use tairix_caps::CapabilitySet;
use tairix_collections::RadixTree;
use tairix_kernel_sec::{GroupId, IdentityTable, UserId};
use tairix_sync::{OnceCell, RwLock, SpinLock};
use tairix_util::fallible::collected;

use tairix_log::{Field, FieldValue, Level, Sink};

use crate::audit::{self, AuditEvent};
use crate::fswatch::ClaimRef;
use crate::wallclock::WallClockSource;

use super::blkmeter::VolumeIoSource;

use super::delegate::{
    map_driver_error, DelegatedEntry, DelegatedFs, DelegatedInfo, DelegatedRef, Expect, FinalLink,
    HeldNode, Links, MissingSteps, Walk,
};
use super::listing::{ListEnd, Listing};
use super::mount::{ChildMounts, Projection};
use super::node::{
    Acting, At, Authority, Cwd, DriverLock, NodeHandle, NodeRef, Start, VolumeNodes,
};
use super::path::{parse_link_target, spell, Path};
use super::perm::{Access, CreateMode, Credentials, Judged, Metadata};
use super::service::{FilesystemService, LookedUp, Opened, ReaddirEntry};
use super::vfs::{
    canonical_answer, canonicalising, spell_canonical, vacancy, CatalogEnd, Drivers, Kept, Landing,
    MountUse, Posture, Resolution, Serve, Spelling,
};
use super::writeback::WritebackDue;
use super::{Vfs, VfsError};

/// The entry of the registration numbered `instance` in `drivers`, which the
/// registry keeps in instance order.
fn registration<F: 'static>(drivers: &[DriverEntry<F>], instance: u64) -> Option<&DriverEntry<F>> {
    drivers
        .binary_search_by_key(&instance, |e| e.nodes.instance())
        .ok()
        .map(|at| &drivers[at])
}

/// One backing filesystem driver registered in a [`LateFilesystem`],
/// addressed by the [`DriverHandle`] its mount carries in the VFS mount
/// table.
///
/// The driver needs `&mut self` per operation and may **park** across a
/// block-device completion IRQ, so it is serialised by a sleeping
/// [`SleepLock`](crate::sleeplock::SleepLock) (never a spin lock — a second contender would busy-spin
/// while the holder sleeps). The lock is shared by [`Arc`] so the hot path
/// clones the handle out of the registry without holding the registry
/// lock across the (possibly parking) operation — and so a runtime
/// [`unregister`](LateFilesystem::unregister) (a hotplug volume detach)
/// drops the registry's reference while any in-flight operation keeps the
/// driver alive through its own clone: no leak per detach, no
/// use-after-free.
struct DriverEntry<F: 'static> {
    handle: u64,
    driver: Arc<DriverLock<F>>,
    /// The backing volume's name (partition label / volume identity), as the
    /// mount snapshot reports it. Registration-time facts, not driver state:
    /// the registrar names what it mounted.
    source: String,
    /// The driver's filesystem-type name (`arxfs`, …).
    fstype: String,
    /// The volume's stable published identity (the same 16 bytes the volume
    /// forest publishes for `id::` paths), or all-zero when the registrar
    /// published none. A registration-time fact like `source`, reported by
    /// the mount snapshot so the unmount tooling can name the volume it
    /// detaches.
    volume_id: [u8; 16],
    /// Whether the backing volume is live. Flipped by
    /// [`LateFilesystem::set_availability`] when a surprise removal parks
    /// the volume behind its fail-closed stand-in, so the mount snapshot
    /// never shows a vanished volume as healthy.
    availability: MountAvailability,
    /// The live block client's reported-health overlay, when the backing
    /// volume is served by a fault-aware block device
    /// (`plans/FIX-IO.md` IO2/IO3). It carries a [`MountAvailability`] wire
    /// byte the block client updates on every completion. When the stored
    /// `availability` is still [`MountAvailability::Available`] (no surprise
    /// removal has parked the volume), the snapshot overlays a live
    /// `Degraded`/`Recovering` reading from here so a live-but-unwell device
    /// never reads as healthy; the authoritative `Unavailable*`/
    /// `RecoveryConflict` vanish states always win over the overlay. `None`
    /// for a volume with no fault-aware block source (the in-RAM layout
    /// mounts, a boot volume over a non-block backing).
    ///
    /// Besides the availability overlay it also carries the serving
    /// block-service endpoint id and every counter the block client folds —
    /// the outcome tallies, the service counters, the queue occupancy and
    /// the budget bounding it — so the three `sysinfo` per-volume queries can
    /// report a device's live readings (`plans/FIX-IO.md` IO5).
    io: Option<VolumeIoSource>,
    /// When this volume's batched filesystem transaction must be published,
    /// as the driver last reported it ([`super::writeback`]). Empty for a
    /// driver that publishes at every operation, and for one that holds
    /// nothing open.
    writeback: WritebackDue,
    /// The claim on the volume's watch table, released when the volume is
    /// unregistered.
    watch: Option<ClaimRef>,
    /// The space figures the volume last reported, for a listing taken
    /// while it is busy.
    usage: Arc<SpinLock<VolumeStats>>,
    /// The registration the volume's descriptors pin through.
    nodes: Arc<VolumeNodes>,
}

/// One registered volume's snapshot facts, as [`LateFilesystem::entries`]
/// reports them to the mount snapshot: the registration names, the shared
/// driver lock, the volume's published identity, and its availability.
struct SnapshotEntry<F: 'static> {
    handle: u64,
    source: String,
    fstype: String,
    driver: Arc<DriverLock<F>>,
    volume_id: [u8; 16],
    availability: MountAvailability,
    usage: Arc<SpinLock<VolumeStats>>,
}

/// The availability the mount snapshot reports for one registered volume:
/// its stored state, with a live block-health overlay applied only when the
/// stored state is [`MountAvailability::Available`].
///
/// The surprise-removal path owns the authoritative `Unavailable*`/
/// `RecoveryConflict` states, so once it has parked a volume the overlay
/// never competes with it. For a still-`Available` volume served by a
/// fault-aware block device, the block client's reported health is reflected
/// so a live-but-unwell device reads as `Degraded`/`Recovering` rather than
/// healthy. A missing or unreadable overlay leaves the volume `Available`
/// (the block client seeds it `Available` and only ever stores a live
/// health state), never a fabricated unavailable reading.
fn overlaid_availability(
    stored: MountAvailability,
    health: Option<&VolumeIoSource>,
) -> MountAvailability {
    if !matches!(stored, MountAvailability::Available) {
        return stored;
    }
    health
        .and_then(VolumeIoSource::live_availability)
        .unwrap_or(MountAvailability::Available)
}

/// A set-once VFS policy layer plus a registry of backing filesystem drivers
/// the boot path installs after the disk(s) come online (mirrors
/// [`crate::users::LateUsersDb`]).
///
/// The syscall layer is built before any disk is mounted, so the handlers
/// hold a `&'static LateFilesystem` from boot; until the VFS is published and
/// the driver of each projection a walk reaches is registered, the operation
/// fails closed with [`Errno::NotImplemented`] — identical to the hollow
/// [`NULL_FILESYSTEM`](super::service::NULL_FILESYSTEM).
///
/// One VFS owns the whole mount table; **several** backing volumes can be
/// registered (e.g. the read-only `/System` volume and the writable
/// `/System/Logs` subtree of the root volume). A walk reads the
/// [`DriverHandle`] of each projection it crosses and runs against the
/// matching driver — so operations on *different* volumes proceed in
/// parallel (each behind its own [`DriverLock`]) and a slow device never
/// stalls an unrelated one.
///
/// `Sync` (through [`OnceCell`], [`SpinLock`], and the per-driver
/// [`DriverLock`]) so the single `&'static` instance is shared by the per-CPU
/// syscall handlers.
pub struct LateFilesystem<F: 'static> {
    /// The shared policy layer: absolute-path resolution, the mount table,
    /// and the per-inode permission model, set once when the layout is known.
    vfs: OnceCell<Vfs>,
    /// The backing drivers, in registration-instance order so a lookup by
    /// registration is a binary search. Added to as each volume comes online
    /// (rare); read on every `fs_*` call. The spin lock is held only for the
    /// tiny lookup/insert, never across a filesystem operation (the driver's
    /// lock is cloned out first), so it never spins on a parked holder.
    drivers: SpinLock<Vec<DriverEntry<F>>>,
    /// The host every registered driver is handed, once the boot path has one
    /// to give ([`Self::install_volume_host`]). Until then a driver is
    /// registered with no host and publishes at every operation, so no
    /// transaction is ever deferred without a timer to fire it.
    volume_host: OnceCell<&'static dyn VolumeHost>,
    /// Where a volume running down to its reserve is recorded, installed with
    /// the host.
    audit: OnceCell<&'static (dyn Sink + Sync)>,
    /// The clock every volume stamps its times from, installed with the host.
    wall_clock: OnceCell<&'static (dyn WallClockSource + 'static)>,
    /// Whether the flusher is live and will publish what a driver defers
    /// ([`Self::arm_writeback`]). Until it is, the host declines to read its
    /// clock, so every driver publishes at each operation: the batching window
    /// exists only once something can fire it.
    writeback_armed: AtomicBool,
    /// The number the next registration is given; never reused.
    instances: AtomicU64,
}

/// An install/registration was refused because the target is already set.
///
/// The VFS cell is immutable after the first successful
/// [`install_vfs`](LateFilesystem::install_vfs), and a [`DriverHandle`] is
/// registered at most once, so neither the live VFS nor a live driver can be
/// replaced by a later code path.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub struct FilesystemAlreadyInstalled;

/// Why a volume's driver could not be registered.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum RegisterError {
    /// The handle is registered under another registration.
    AlreadyRegistered,
    /// The heap could not hold the registration.
    OutOfMemory,
}

impl<F: FilesystemWrite + Send + 'static> LateFilesystem<F> {
    /// Construct an empty cell. `const` so a boot path can place it in a
    /// `static` and hand `&LATE_FILESYSTEM` to the handler builder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            vfs: OnceCell::new(),
            drivers: SpinLock::new(Vec::new()),
            volume_host: OnceCell::new(),
            audit: OnceCell::new(),
            wall_clock: OnceCell::new(),
            writeback_armed: AtomicBool::new(false),
            instances: AtomicU64::new(1),
        }
    }

    /// Publish the shared VFS policy layer exactly once.
    ///
    /// The VFS mount table already names the [`DriverHandle`] of every
    /// backing volume; [`register`](Self::register) attaches each live driver
    /// to its handle (before or after this call — a request for a not-yet-
    /// registered handle fails closed until it lands).
    ///
    /// # Errors
    ///
    /// [`FilesystemAlreadyInstalled`] if a VFS is already installed.
    pub fn install_vfs(&self, vfs: Vfs) -> Result<(), FilesystemAlreadyInstalled> {
        self.vfs.set(vfs).map_err(|_| FilesystemAlreadyInstalled)
    }

    /// Register the live driver backing the mount addressed by `handle`,
    /// naming the backing volume (`source`) and the driver's filesystem type
    /// (`fstype`) for the mount snapshot, under a fresh registration.
    ///
    /// The driver is wrapped in a [`DriverLock`] behind an [`Arc`], so the
    /// hot path can clone the handle out of the registry without holding
    /// the registry lock across a parking operation, and a runtime
    /// [`unregister`](Self::unregister) can drop the registry's reference
    /// while in-flight operations finish on their own clones.
    ///
    /// Returns a clone of the shared lock so a kernel-internal consumer that
    /// must write the same volume (the `CAP_USER_ADMIN`
    /// account-administration engine's storage) can share the **one** live
    /// driver instance: a volume has exactly one writer, and every mutation
    /// serialises through this lock — a second independent driver over the
    /// same device would corrupt its copy-on-write allocation state.
    ///
    /// # Errors
    ///
    /// [`RegisterError::AlreadyRegistered`] if `handle` is already registered
    /// — a driver is bound to its handle exactly once while registered (fail
    /// closed; never a silent re-bind) — and [`RegisterError::OutOfMemory`]
    /// when the heap cannot hold the registration. A handle freed by
    /// [`unregister`](Self::unregister) may be reused by a later attach.
    pub fn register(
        &self,
        handle: DriverHandle,
        driver: F,
        source: &str,
        fstype: &str,
        volume_id: [u8; 16],
        watch: Option<ClaimRef>,
    ) -> Result<Arc<DriverLock<F>>, RegisterError> {
        let instance = self.instances.fetch_add(1, Ordering::Relaxed);
        let nodes =
            Arc::try_new(VolumeNodes::new(instance)).map_err(|_| RegisterError::OutOfMemory)?;
        self.register_with_nodes(handle, driver, source, fstype, volume_id, watch, nodes)
    }

    /// [`register`](Self::register) `driver` under the existing registration
    /// `nodes`: the stand-in for a volume that vanished with work unpublished
    /// takes over the registration `handle` serves, so the descriptors opened
    /// through it answer the stand-in while the volume is away. With `handle`
    /// registered under `nodes`, its driver is replaced in place, so no
    /// operation finds the handle unregistered in between.
    ///
    /// # Errors
    ///
    /// As [`register`](Self::register): [`RegisterError::AlreadyRegistered`]
    /// if `handle` is registered under another registration.
    // What `register` names the volume by, and the registration it is filed
    // under.
    #[allow(clippy::too_many_arguments)]
    pub fn register_with_nodes(
        &self,
        handle: DriverHandle,
        driver: F,
        source: &str,
        fstype: &str,
        volume_id: [u8; 16],
        watch: Option<ClaimRef>,
        nodes: Arc<VolumeNodes>,
    ) -> Result<Arc<DriverLock<F>>, RegisterError> {
        let raw = handle.as_u64();
        let oom = |_| RegisterError::OutOfMemory;
        // Everything the entry holds is allocated before the registry's lock
        // is taken, so a heap that refuses leaves the registry as it was.
        let shared = Arc::try_new(DriverLock::new(driver, Arc::clone(&nodes))).map_err(oom)?;
        let usage = Arc::try_new(SpinLock::new(VolumeStats::default())).map_err(oom)?;
        let owned = |text: &str| {
            let mut owned = String::new();
            owned
                .try_reserve_exact(text.len())
                .map_err(|_| RegisterError::OutOfMemory)?;
            owned.push_str(text);
            Ok::<_, RegisterError>(owned)
        };
        let entry = DriverEntry {
            handle: raw,
            driver: Arc::clone(&shared),
            source: owned(source)?,
            fstype: owned(fstype)?,
            volume_id,
            availability: MountAvailability::Available,
            io: None,
            writeback: WritebackDue::empty(),
            watch,
            usage,
            nodes,
        };
        let replaced = {
            let mut drivers = self.drivers.lock();
            let at = drivers.iter().position(|e| e.handle == raw);
            if at.is_some_and(|at| !Arc::ptr_eq(&drivers[at].nodes, &entry.nodes)) {
                return Err(RegisterError::AlreadyRegistered);
            }
            if let Some(at) = at {
                Some(core::mem::replace(&mut drivers[at], entry))
            } else {
                drivers
                    .try_reserve(1)
                    .map_err(|_| RegisterError::OutOfMemory)?;
                let instance = entry.nodes.instance();
                let at = drivers.partition_point(|e| e.nodes.instance() < instance);
                drivers.insert(at, entry);
                None
            }
        };
        // The replaced driver's watchers learn it left, as an unregister's do,
        // and it goes outside the registry's lock.
        if let Some(watch) = replaced.as_ref().and_then(|old| old.watch.as_ref()) {
            watch.release();
        }
        drop(replaced);
        // The entry is in place before the driver learns of the timer, so a
        // driver that reports a deadline from inside this call has a slot to
        // report it into. This acquire cannot park and cannot spin: the lock
        // was created above and its handle has not left this function, so the
        // fast path takes it — which matters because the surprise-removal path
        // registers a stand-in from a context that must not park, and
        // because the registry's own spin lock is released first either way.
        if let Ok(Some(host)) = self.volume_host.get() {
            shared.lock().set_volume_host(handle, *host);
        }
        // What the mount listing reports of the volume moved — its name,
        // type, space and availability — though the table's own mounts did
        // not: an unlocked root is exactly this.
        crate::notice::mounts_changed();
        Ok(shared)
    }

    /// The registration the volume registered for `handle` pins through.
    #[must_use]
    pub fn nodes(&self, handle: DriverHandle) -> Option<Arc<VolumeNodes>> {
        let raw = handle.as_u64();
        self.drivers
            .lock()
            .iter()
            .find(|e| e.handle == raw)
            .map(|e| Arc::clone(&e.nodes))
    }

    /// The driver and published identity of the volume whose registration is
    /// numbered `instance`, while it is registered.
    fn registered(&self, instance: u64) -> Option<(Arc<DriverLock<F>>, [u8; 16])> {
        let drivers = self.drivers.lock();
        registration(&drivers, instance).map(|e| (Arc::clone(&e.driver), e.volume_id))
    }

    /// The watch table the registration numbered `instance` claimed, while it
    /// is registered.
    fn watch_table(&self, instance: u64) -> Option<Arc<crate::fswatch::VolumeWatch>> {
        let drivers = self.drivers.lock();
        registration(&drivers, instance)
            .and_then(|e| e.watch.as_ref())
            .map(|claim| Arc::clone(claim.table()))
    }

    /// What serves a backed projection of `handle`: its driver, its
    /// registration, and its published identity, read under one hold of the
    /// registry's lock.
    fn serving(&self, handle: DriverHandle) -> Result<ServedVolume<F>, Errno> {
        let raw = handle.as_u64();
        self.drivers
            .lock()
            .iter()
            .find(|e| e.handle == raw)
            .map(|e| ServedVolume {
                driver: Arc::clone(&e.driver),
                nodes: Arc::clone(&e.nodes),
                volume_id: e.volume_id,
            })
            .ok_or(Errno::NotImplemented)
    }

    /// Tell `volume`'s driver about the pins its descriptors released, if the
    /// driver's lock is free this instant: a close never waits behind a long
    /// operation, and the write-back flusher reaps what this leaves. The
    /// releases of a registration no longer registered go with it.
    pub fn reap(&self, volume: &VolumeNodes)
    where
        F: FilesystemRead,
    {
        if let Some((driver, _)) = self.registered(volume.instance()) {
            if let Some(mut fs) = driver.try_lock() {
                if volume.take_reap_due() {
                    volume.reap(&mut *fs);
                }
            }
        }
    }

    /// Reap every registration a release left due whose driver is free: the
    /// write-back flusher's pass. Registrations are visited in instance order
    /// and one is taken from the registry at a time, so the pass allocates
    /// nothing and one registered or withdrawn beneath it moves none of the
    /// others. A volume busy elsewhere stays due, and the hold that frees it
    /// wakes the flusher again.
    pub(super) fn reap_due(&self)
    where
        F: FilesystemRead,
    {
        let mut after = None;
        while let Some((driver, nodes)) = self.next_reap_due(after) {
            after = Some(nodes.instance());
            if let Some(mut fs) = driver.try_lock() {
                if nodes.take_reap_due() {
                    nodes.reap(&mut *fs);
                }
            }
        }
    }

    /// The due registration with the lowest instance number above `after`.
    fn next_reap_due(&self, after: Option<u64>) -> Option<(Arc<DriverLock<F>>, Arc<VolumeNodes>)> {
        let drivers = self.drivers.lock();
        let from = after.map_or(0, |after| {
            drivers.partition_point(|e| e.nodes.instance() <= after)
        });
        drivers[from..]
            .iter()
            .find(|e| e.nodes.is_reap_due())
            .map(|e| (Arc::clone(&e.driver), Arc::clone(&e.nodes)))
    }

    /// Publish the host every driver registered from now on is handed, so a
    /// filesystem that batches commits has something above it to publish a
    /// volume that falls quiet ([`super::writeback`]), with `audit`, where a
    /// volume running down to its reserve is recorded, and `wall_clock`, which
    /// every volume stamps its times from.
    ///
    /// Set-once per boot, and installed before any writable volume is
    /// registered: a driver registered without a host publishes at every
    /// operation, which is slower but never defers durability with no timer
    /// to fire it (fail closed), and stamps no time.
    ///
    /// # Errors
    ///
    /// [`FilesystemAlreadyInstalled`] if a host is already installed — the
    /// live timer is never re-pointed.
    pub fn install_volume_host(
        &self,
        host: &'static dyn VolumeHost,
        audit: &'static (dyn Sink + Sync),
        wall_clock: &'static (dyn WallClockSource + 'static),
    ) -> Result<(), FilesystemAlreadyInstalled> {
        self.volume_host
            .set(host)
            .map_err(|_| FilesystemAlreadyInstalled)?;
        let _ = self.audit.set(audit);
        let _ = self.wall_clock.set(wall_clock);
        Ok(())
    }

    /// Record that the write-back flusher is live and will publish what a
    /// driver defers: it has proved it can park and be woken, and parks for
    /// good. Until then the host reads no clock, so every driver publishes
    /// rather than deferring against a timer that would not fire.
    pub fn arm_writeback(&self) {
        self.writeback_armed.store(true, Ordering::Release);
    }

    /// Record the write-back deadline volume `handle` just published, and ask
    /// for the flusher to be woken when it is sooner than the deadline the
    /// flusher is already armed for.
    ///
    /// Called from inside the driver, under the mount lock, so it takes only
    /// the registry's short lookup lock and performs one atomic store — never
    /// a park, an allocation, or any I/O. An unknown handle records nothing:
    /// a driver reporting against a handle this registry does not hold has no
    /// mount here to publish.
    pub fn note_writeback_due(&self, handle: DriverHandle, deadline_ns: Option<u64>) {
        let raw = handle.as_u64();
        let recorded = match self.drivers.lock().iter().find(|e| e.handle == raw) {
            Some(entry) => {
                entry.writeback.store(deadline_ns);
                true
            }
            None => false,
        };
        // Outside the registry lock: the wake reads the write-back queue's
        // own lock, and no lock is held across another.
        if recorded {
            crate::waitq::writeback_wake(deadline_ns);
        }
    }

    /// Whether the volume registered for `handle` holds a transaction its
    /// driver has not published: writes it acknowledged that its device has
    /// not been given. Takes only the registry's short lookup lock, so the
    /// surprise-removal path may ask it.
    #[must_use]
    pub fn holds_unpublished(&self, handle: DriverHandle) -> bool {
        let raw = handle.as_u64();
        self.drivers
            .lock()
            .iter()
            .find(|e| e.handle == raw)
            .is_some_and(|e| e.writeback.holds_open())
    }

    /// The soonest write-back deadline any registered volume has published,
    /// or `None` when no volume holds an open transaction — the deadline the
    /// flusher parks until. A linear read of a handful of mounts, off every
    /// hot path.
    #[must_use]
    pub fn earliest_writeback_due(&self) -> Option<u64> {
        self.drivers
            .lock()
            .iter()
            .filter_map(|e| e.writeback.load())
            .min()
    }

    /// Every registered volume whose write-back deadline has arrived by
    /// `now_ns`, in deadline order, each with its deadline **consumed**.
    ///
    /// Consuming under the registry lock is what keeps two flusher passes
    /// from publishing one volume twice, and what stops a fired deadline
    /// re-arming the timer in the past. The drivers are returned as shared
    /// handles so the caller flushes them with the registry lock released
    /// (the flush parks on device I/O).
    pub fn take_writeback_due(&self, now_ns: u64) -> Vec<(DriverHandle, Arc<DriverLock<F>>)> {
        let mut due: Vec<(u64, u64, Arc<DriverLock<F>>)> = self
            .drivers
            .lock()
            .iter()
            .filter_map(|e| {
                let deadline = e.writeback.take_if_due(now_ns)?;
                Some((deadline, e.handle, Arc::clone(&e.driver)))
            })
            .collect();
        due.sort_unstable_by_key(|&(deadline, handle, _)| (deadline, handle));
        due.into_iter()
            .filter_map(|(_, handle, driver)| {
                DriverHandle::from_raw(handle)
                    .ok()
                    .map(|handle| (handle, driver))
            })
            .collect()
    }

    /// Record the availability of the volume registered for `handle`, so
    /// the mount snapshot reports a surprise-removed volume as unavailable
    /// rather than healthy.
    ///
    /// # Errors
    ///
    /// [`Errno::NotImplemented`] when `handle` names no registered volume
    /// (fail closed — nothing is marked).
    pub fn set_availability(
        &self,
        handle: DriverHandle,
        availability: MountAvailability,
    ) -> Result<(), Errno> {
        let handle = handle.as_u64();
        let mut drivers = self.drivers.lock();
        let entry = drivers
            .iter_mut()
            .find(|e| e.handle == handle)
            .ok_or(Errno::NotImplemented)?;
        if entry.availability != availability {
            entry.availability = availability;
            crate::notice::mounts_changed();
        }
        Ok(())
    }

    /// Attach the served device's live readings `io` to the volume registered
    /// for `handle`, so the mount snapshot reflects the backing device's
    /// reported health (`Degraded`/`Recovering`) while the volume is still
    /// [`MountAvailability::Available`] (`plans/FIX-IO.md` IO2/IO3) and the
    /// per-volume queries can report its counters.
    ///
    /// The handle carries a [`MountAvailability`] wire byte and the counters
    /// the block client updates on every attempt; the registry only reads
    /// them. Idempotent — re-registering a recovered volume replaces them.
    ///
    /// # Errors
    ///
    /// [`Errno::NotImplemented`] when `handle` names no registered volume
    /// (fail closed — nothing is attached).
    pub fn set_io_source(&self, handle: DriverHandle, io: VolumeIoSource) -> Result<(), Errno> {
        let handle = handle.as_u64();
        let mut drivers = self.drivers.lock();
        let entry = drivers
            .iter_mut()
            .find(|e| e.handle == handle)
            .ok_or(Errno::NotImplemented)?;
        entry.io = Some(io);
        Ok(())
    }

    /// A snapshot of every fault-aware block-backed volume's live I/O health,
    /// one [`VolumeIoHealthRecord`] per registered volume that has a block
    /// health source, for the `sysinfo` volume-health query
    /// (`plans/FIX-IO.md` IO5).
    ///
    /// A volume with no fault-aware block source (the in-RAM layout mounts, a
    /// boot volume over a non-block backing) is omitted rather than reported
    /// with fabricated health — the query lists exactly the devices whose
    /// health the kernel actually observes. The order is the registry's
    /// registration order, stable across a walk. Each record overlays the
    /// live availability the same way the mount snapshot does, names the
    /// serving endpoint, and carries the counters snapshot; it holds no
    /// secret.
    fn volume_io_health_records(&self) -> Vec<VolumeIoHealthRecord> {
        self.io_records(|entry, source| {
            VolumeIoHealthRecord::new(
                entry.volume_id,
                source.dev,
                overlaid_availability(entry.availability, Some(source)),
                source.counters.snapshot(),
            )
        })
    }

    /// A snapshot of every fault-aware block-backed volume's cumulative I/O
    /// service counters, one [`VolumeIoStatsRecord`] per volume with a block
    /// I/O source, in the same order and with the same omissions as
    /// [`volume_io_health_records`](Self::volume_io_health_records).
    fn volume_io_stats_records(&self) -> Vec<VolumeIoStatsRecord> {
        self.io_records(|entry, source| {
            VolumeIoStatsRecord::new(
                entry.volume_id,
                source.dev,
                source.stats.io_snapshot(),
                source.device,
            )
        })
    }

    /// A snapshot of every fault-aware block-backed volume's live queue
    /// occupancy and the budget bounding it, one [`VolumeIoQueueRecord`] per
    /// volume with a block I/O source, in the same order and with the same
    /// omissions as
    /// [`volume_io_health_records`](Self::volume_io_health_records).
    fn volume_io_queue_records(&self) -> Vec<VolumeIoQueueRecord> {
        self.io_records(|entry, source| {
            VolumeIoQueueRecord::new(
                entry.volume_id,
                source.dev,
                source.stats.queue_snapshot(),
                source.budget,
            )
        })
    }

    /// Walk the driver registry once and build one record per registered
    /// volume that has a block I/O source, through `record`.
    ///
    /// The one definition of that walk, so the three per-volume queries
    /// cannot diverge in which volumes they list or in what order: a volume
    /// with no fault-aware block source (the in-RAM layout mounts, a boot
    /// volume over a non-block backing) is omitted from all three rather than
    /// reported with fabricated readings, and the order is the registry's
    /// registration order, stable across a walk. A client can therefore join
    /// the three lists by `volume_id`.
    fn io_records<R>(&self, record: impl Fn(&DriverEntry<F>, &VolumeIoSource) -> R) -> Vec<R> {
        self.drivers
            .lock()
            .iter()
            .filter_map(|entry| {
                let source = entry.io.as_ref()?;
                Some(record(entry, source))
            })
            .collect()
    }

    /// Run `remove` — the orderly hardware-tree removal closure — but only
    /// while no registered volume is served from one of `endpoints`, holding
    /// the driver-registry lock across **both** the busy check and `remove`.
    ///
    /// That single lock acquisition is the atomicity guarantee: an attach
    /// registers its volume under the same lock ([`register`](Self::register)
    /// / [`set_availability`](Self::set_availability)), so no attach can land
    /// between the check and the removal. A volume whose serving
    /// block-service endpoint id (`health.dev`) is in `endpoints` makes the
    /// node busy: the method returns [`Errno::Busy`] and **never calls
    /// `remove`**, so nothing is retired (fail closed). A registered volume
    /// with no fault-aware block source (an in-RAM mount) has no serving
    /// endpoint and can never make a node busy.
    ///
    /// The lock order is registry → hardware tree (the `remove` closure takes
    /// the hardware-tree store's lock while this holds the registry lock);
    /// the hardware-tree store never reaches back into the filesystem
    /// registry, so there is no reverse path and no deadlock. The closure is
    /// a short in-memory tree edit that never parks, so holding the registry
    /// spin lock across it is safe.
    fn remove_if_endpoints_idle(
        &self,
        endpoints: &[u64],
        remove: &mut dyn FnMut() -> Result<Vec<u32>, Errno>,
    ) -> Result<Vec<u32>, Errno> {
        let drivers = self.drivers.lock();
        let busy = drivers
            .iter()
            .filter_map(|entry| entry.io.as_ref())
            .any(|source| endpoints.contains(&source.dev));
        if busy {
            return Err(Errno::Busy);
        }
        remove()
    }

    /// Withdraw the driver registered for `handle` (a runtime volume
    /// detach), retiring its registration and returning the registry's
    /// shared handle so the caller can flush and drop it.
    ///
    /// Operations already in flight hold their own [`Arc`] clones and
    /// finish safely; every later resolution of `handle` fails closed
    /// [`Errno::NotImplemented`] exactly as before the driver was
    /// registered, and every descriptor opened through the registration
    /// answers [`Errno::Stale`]. Fails closed: an unknown handle removes
    /// nothing.
    pub fn unregister(&self, handle: DriverHandle) -> Option<Arc<DriverLock<F>>> {
        let handle = handle.as_u64();
        let entry = {
            let mut drivers = self.drivers.lock();
            let pos = drivers.iter().position(|e| e.handle == handle)?;
            drivers.remove(pos)
        };
        entry.nodes.retire();
        // The volume's watchers learn it left now, not when its last operation
        // in flight lets the driver go.
        if let Some(watch) = &entry.watch {
            watch.release();
        }
        crate::notice::mounts_changed();
        Some(entry.driver)
    }

    /// Whether the shared VFS has been installed.
    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.vfs.is_initialised()
    }

    /// The installed VFS, or [`Errno::NotImplemented`] before one is
    /// published (fail closed — a kernel with no mounted volume serves no
    /// `fs_*` syscall). Public for the runtime volume attach/detach
    /// service, which adds and retracts mounts through the one live mount
    /// table.
    pub fn vfs(&self) -> Result<&Vfs, Errno> {
        match self.vfs.get() {
            Ok(Some(vfs)) => Ok(vfs),
            _ => Err(Errno::NotImplemented),
        }
    }

    /// The driver registered for `handle`, or [`Errno::NotImplemented`] when
    /// none is (fail closed — a mount whose backing volume is not yet online,
    /// or has no driver, serves no operation, never a silent fallback).
    /// Public for the runtime volume detach path, which flushes exactly the
    /// departing volume before retracting it.
    pub fn driver(&self, handle: DriverHandle) -> Result<Arc<DriverLock<F>>, Errno> {
        let handle = handle.as_u64();
        let drivers = self.drivers.lock();
        drivers
            .iter()
            .find(|e| e.handle == handle)
            .map(|e| Arc::clone(&e.driver))
            .ok_or(Errno::NotImplemented)
    }

    /// Every registered driver, for a whole-system `sync`.
    fn all_drivers(&self) -> Vec<Arc<DriverLock<F>>> {
        self.drivers
            .lock()
            .iter()
            .map(|e| Arc::clone(&e.driver))
            .collect()
    }

    /// Every registered volume's snapshot facts, sorted by handle, read under
    /// one hold of the registry lock.
    fn entries(&self) -> Vec<SnapshotEntry<F>> {
        let mut entries: Vec<SnapshotEntry<F>> = self
            .drivers
            .lock()
            .iter()
            .map(|e| SnapshotEntry {
                handle: e.handle,
                source: e.source.clone(),
                fstype: e.fstype.clone(),
                driver: Arc::clone(&e.driver),
                volume_id: e.volume_id,
                availability: overlaid_availability(e.availability, e.io.as_ref()),
                usage: Arc::clone(&e.usage),
            })
            .collect();
        entries.sort_unstable_by_key(|entry| entry.handle);
        entries
    }
}

impl<F: FilesystemWrite + Send + 'static> Default for LateFilesystem<F> {
    fn default() -> Self {
        Self::new()
    }
}

/// The registry *is* the write-back timer's bookkeeping: it already keys
/// every mounted volume by the handle a driver reports against, so the
/// deadline lives beside the driver it belongs to rather than in a second
/// table that could drift out of step with the mounts.
impl<F: FilesystemWrite + Send + 'static> VolumeHost for LateFilesystem<F> {
    fn now_ns(&self) -> Option<u64> {
        // One atomic load on the driver's per-operation path. A driver may
        // only measure a window while the flusher that will fire it is live.
        if !self.writeback_armed.load(Ordering::Acquire) {
            return None;
        }
        crate::waitq::wait_now_ns()
    }

    /// A stamp outlives the boot and carries no provenance, so only a reading
    /// the clock can vouch for is handed out: one a source has set and that
    /// is plausible, which a dead clock battery's year 2000 is not.
    fn wall_time(&self) -> Option<Time64> {
        let clock = self.wall_clock.get().ok().flatten()?;
        let reading = clock.read(crate::waitq::wait_now_ns()?);
        (reading.state().is_set() && is_plausible_wall_time(reading.time())).then(|| reading.time())
    }

    fn writeback_due(&self, volume: DriverHandle, deadline_ns: Option<u64>) {
        self.note_writeback_due(volume, deadline_ns);
    }

    /// The mount listing reports the volume's space, so its subscribers hear
    /// of the crossing through the `Mounts` notice; the record is the
    /// operator's warning while the system still has runway.
    fn reserve_crossed(&self, volume: DriverHandle, in_reserve: bool) {
        let source = self
            .drivers
            .lock()
            .iter()
            .find(|e| e.handle == volume.as_u64())
            .map(|e| e.source.clone());
        if let (Ok(Some(audit)), Some(source)) = (self.audit.get(), source) {
            let (level, event) = if in_reserve {
                (Level::Warn, AuditEvent::VolumeReserveEntered)
            } else {
                (Level::Info, AuditEvent::VolumeReserveLeft)
            };
            audit::emit(
                *audit,
                level,
                event,
                &[
                    Field {
                        key: "volume",
                        value: FieldValue::UnsignedInt(volume.as_u64()),
                    },
                    Field {
                        key: "source",
                        value: FieldValue::Str(&source),
                    },
                ],
            );
        }
        crate::notice::mounts_changed();
    }
}

/// A cell holding the authoritative user/group identity table the `fs_*`
/// path resolves caller groups against (mirrors
/// [`crate::users::LateUsersDb`]).
///
/// The on-disk accounts are read only after the root is mounted,
/// past the point where the handler set is built, so the service holds a
/// `&'static LateIdentity` from boot and the trusted unlock step installs the
/// verified [`IdentityTable`] once it exists. The table is **credential-free**
/// (it carries the uid → group/capability mapping the VFS needs, never the
/// salted password records the `users_db_read` text path serves), so a
/// long-lived `&'static` copy leaks no secret.
///
/// [`install`](Self::install) is set-once: the boot unlock publishes the
/// first table and every later install is refused. The only path that
/// changes the table afterwards is [`replace`](Self::replace), called
/// exclusively by the `CAP_USER_ADMIN` admin engine after it has
/// validated, re-verified, and persisted an edited account database
/// (`plans/CAPABILITY_USE.md` CU4). Filesystem checks read groups from the
/// table in force, so an edit binds them at a principal's next operation;
/// what a spawn snapshots onto a task binds at the next spawn or login.
///
/// Until [`install`](Self::install), resolving a uid fails closed with
/// [`Errno::NotImplemented`]; an attested uid with no account is denied with
/// [`Errno::PermissionDenied`]. Neither path ever invents a principal.
pub struct LateIdentity {
    table: RwLock<Option<IdentityTable>>,
    /// Moves with every install and replacement, under the table's write
    /// lock, so what was resolved from one table is never taken for
    /// another's.
    generation: AtomicU64,
}

/// A principal's primary and supplementary groups, as one resolution read
/// them.
#[derive(Clone, Debug)]
struct Groups {
    primary: GroupId,
    supplementary: Arc<Vec<GroupId>>,
}

/// An identity-table install was refused because one is already installed.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub struct IdentityAlreadyInstalled;

impl LateIdentity {
    /// Construct an empty cell. `const` so a boot path can place it in a
    /// `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            table: RwLock::new(None),
            generation: AtomicU64::new(0),
        }
    }

    /// Publish the verified identity table exactly once.
    ///
    /// # Errors
    ///
    /// [`IdentityAlreadyInstalled`] if a table is already installed — the
    /// boot unlock is the only path that may publish the *first* table;
    /// later changes go through the audited [`replace`](Self::replace)
    /// path alone.
    pub fn install(&self, table: IdentityTable) -> Result<(), IdentityAlreadyInstalled> {
        let mut held = self.table.write();
        if held.is_some() {
            return Err(IdentityAlreadyInstalled);
        }
        *held = Some(table);
        self.generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Replace the installed table with a re-verified one
    /// (`plans/CAPABILITY_USE.md` CU4).
    ///
    /// Called exclusively by the `CAP_USER_ADMIN` admin engine after the
    /// edited databases passed the same verifying build as the boot load;
    /// never a user-reachable install path.
    ///
    /// # Errors
    ///
    /// [`Errno::NotImplemented`] when no table has been installed yet: a
    /// replacement can never create the first table (fail closed).
    pub fn replace(&self, table: IdentityTable) -> Result<(), Errno> {
        let mut held = self.table.write();
        if held.is_none() {
            return Err(Errno::NotImplemented);
        }
        *held = Some(table);
        self.generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// How many times a table was installed or replaced.
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Whether an identity table has been installed.
    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.table.read().is_some()
    }

    /// Resolve the attested `uid`'s group credential as a shared snapshot
    /// (primary gid, supplementary gids), with the generation of the table
    /// it was read from.
    ///
    /// Shared rather than borrowed so no read borrow is held across a
    /// filesystem operation (which may park on device completion) while
    /// the table stays replaceable underneath.
    ///
    /// The **system principal** (`uid 0`) is kernel-defined, not
    /// database-defined: it exists before any account table can be read
    /// (PID 1 and the boot services must load their store bundles off the
    /// read-only `/System` volume before the root is mounted)
    /// and on an installer image no table ever defines it. It therefore
    /// resolves to the same capability-less bootstrap identity the boot
    /// readers use (`gid 0`, no supplementary groups) whenever the table is
    /// absent or holds no `uid 0` record; a table record for `uid 0` (the
    /// compiled-in `system` account) wins when present. The fallback
    /// grants no ambient power: every per-inode owner/mode/ACL and
    /// mount-flag check still applies, and `uid 0` tasks exist only through
    /// kernel-attested spawn.
    ///
    /// # Errors
    ///
    /// * [`Errno::NotImplemented`] for a non-zero `uid` before a table is
    ///   installed (the disk has not been unlocked/read yet) — fail closed,
    ///   never resolve.
    /// * [`Errno::PermissionDenied`] when the table holds no account for a
    ///   non-zero `uid` — an unknown principal is denied, never granted a
    ///   guessed identity, and the refusal does not distinguish "unknown
    ///   uid" so it cannot be used to probe for valid ids.
    /// * [`Errno::OutOfMemory`] when the list cannot be copied.
    fn resolve_groups(&self, uid: u32) -> Result<(Groups, u64), Errno> {
        let table = self.table.read();
        // Bumped only under the write lock, so this is the read table's.
        let generation = self.generation.load(Ordering::Relaxed);
        let (primary, supplementary): (GroupId, &[GroupId]) = match &*table {
            Some(table) => match table.user(UserId(uid)) {
                Ok(record) => (record.primary_gid, &record.supplementary_gids),
                Err(_) if uid == 0 => (GroupId(0), &[]),
                Err(_) => return Err(Errno::PermissionDenied),
            },
            None if uid == 0 => (GroupId(0), &[]),
            None => return Err(Errno::NotImplemented),
        };
        let mut list = Vec::new();
        list.try_reserve_exact(supplementary.len())
            .map_err(|_| Errno::OutOfMemory)?;
        list.extend_from_slice(supplementary);
        let groups = Groups {
            primary,
            supplementary: Arc::try_new(list).map_err(|_| Errno::OutOfMemory)?,
        };
        Ok((groups, generation))
    }

    /// Resolve the attested credential — primary group, supplementary
    /// groups, and the account's capability ceiling — for `uid` from the
    /// installed identity table.
    ///
    /// This is the spawn-as-user resolver: when a privileged spawner switches
    /// a child into a target user, the kernel snapshots that user's full group
    /// set **and** its `capability_grants` ceiling onto the child's capability
    /// record from the table it vouches for, so the child's later filesystem
    /// checks run under an authoritative, caller-independent credential and
    /// its effective capability set is derived as `manifest ∩ ceiling`
    /// (`plans/CAPABILITY_USE.md` CU1). The returned values are owned (a
    /// snapshot), not a borrow into the table, so they can be stored on the
    /// task.
    ///
    /// # Errors
    ///
    /// Fails closed exactly as the internal group resolution does:
    /// [`Errno::NotImplemented`] before a table is installed and
    /// [`Errno::PermissionDenied`] for a uid with no account, so a switch to
    /// an unknown or unresolvable user never invents a credential.
    pub fn resolve_credential(
        &self,
        uid: u32,
    ) -> Result<(GroupId, Vec<GroupId>, CapabilitySet), Errno> {
        match &*self.table.read() {
            Some(table) => {
                let record = table
                    .user(UserId(uid))
                    .map_err(|_| Errno::PermissionDenied)?;
                Ok((
                    record.primary_gid,
                    record.supplementary_gids.clone(),
                    record.capability_grants,
                ))
            }
            None => Err(Errno::NotImplemented),
        }
    }
}

impl Default for LateIdentity {
    fn default() -> Self {
        Self::new()
    }
}

/// The production [`FilesystemService`]: serves the `fs_*` syscalls against a
/// late-installed mount, resolving caller groups from a late-installed
/// authoritative identity table.
///
/// Holds the mount cell, the identity cell and the groups it resolved from
/// the second, and adds no authority of its own; every check stays
/// kernel-side in the secured VFS and fails closed.
pub struct MountedFilesystemService<F: 'static> {
    /// The mounted volume the operations run against.
    mount: &'static LateFilesystem<F>,
    /// The authoritative identity table caller groups are resolved against.
    identity: &'static LateIdentity,
    /// The groups each principal acted as, so an operation's common case
    /// resolves and allocates nothing. A leaf: only the kernel heap's own
    /// lock is taken beneath it, by an insertion.
    groups: SpinLock<GroupsCache>,
}

/// The groups of each principal the service acted as, all from the identity
/// table of one generation.
struct GroupsCache {
    generation: u64,
    by_uid: RadixTree<Groups>,
}

impl<F: 'static> MountedFilesystemService<F> {
    /// Build the service over the boot-installed mount and identity cells.
    #[must_use]
    pub const fn new(mount: &'static LateFilesystem<F>, identity: &'static LateIdentity) -> Self {
        Self {
            mount,
            identity,
            groups: SpinLock::new(GroupsCache {
                generation: 0,
                by_uid: RadixTree::new(),
            }),
        }
    }

    /// The attested `uid`'s groups from the table in force: kept from the
    /// last resolution under it, else resolved outside the cache's lock and
    /// kept. A cache that cannot grow resolves again next time.
    ///
    /// # Errors
    ///
    /// As [`LateIdentity::resolve_groups`].
    fn groups_of(&self, uid: u32) -> Result<Groups, Errno> {
        let generation = self.identity.generation();
        let stale = {
            let mut cache = self.groups.lock();
            if cache.generation == generation {
                if let Some(groups) = cache.by_uid.get(u64::from(uid)) {
                    return Ok(groups.clone());
                }
                None
            } else {
                cache.generation = generation;
                Some(core::mem::replace(&mut cache.by_uid, RadixTree::new()))
            }
        };
        // What another table resolved is freed outside the lock.
        drop(stale);
        let (groups, resolved_at) = self.identity.resolve_groups(uid)?;
        let mut cache = self.groups.lock();
        if cache.generation == resolved_at {
            let _ = cache.by_uid.try_insert(u64::from(uid), groups.clone());
        }
        Ok(groups)
    }
}

impl<F> MountedFilesystemService<F>
where
    F: FilesystemRead + FilesystemWrite + FilesystemSecurity + FilesystemStats + Send + 'static,
{
    /// The VFS, and the groups `uid` acts in from the identity table in
    /// force.
    fn acting_in(&self, uid: u32) -> Result<(&Vfs, Groups), Errno> {
        Ok((self.mount.vfs()?, self.groups_of(uid)?))
    }

    /// Resolve `way` as `uid` holding `caps`, and run `on_volume` on what it
    /// reached — once `purpose` passes the flags of the projection it ended
    /// in — under the hold of the driver serving that projection, or
    /// `on_catalog` where it ended in a projection no volume backs.
    ///
    /// The caller's [`Credentials`] are built here from the kernel-attested
    /// `uid`/`caps` and the authoritative identity table, so an operation
    /// never sees a caller-supplied identity.
    fn resolved<T>(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        way: Way<'_>,
        purpose: MountUse,
        on_volume: impl FnOnce(&Vfs, &mut F, &Served, &Credentials<'_>, Landing) -> Result<T, VfsError>,
        on_catalog: impl FnOnce(&Vfs, &Credentials<'_>, CatalogEnd) -> Result<T, VfsError>,
    ) -> Result<T, Errno> {
        let (vfs, groups) = self.acting_in(uid)?;
        let cred = credentials(uid, &groups, caps);
        let resolution =
            Resolution::new(vfs, way.at, way.posture, way.spelling).map_err(VfsError::to_errno)?;
        vfs.resolve::<_, T>(
            &cred,
            &mut Registry { mount: self.mount },
            resolution,
            |fs, served, landing| {
                purpose.admitted_by(&landing.projection)?;
                on_volume(vfs, fs, served, &cred, landing)
            },
            |end| on_catalog(vfs, &cred, end),
        )
        .map_err(VfsError::to_errno)
    }

    /// The names of the directory `handle` holds, from the view root down:
    /// its label while resolving the label by physical names alone reaches
    /// the very node `handle` holds, else the names found climbing from it
    /// to its projection's floor, each in its parent's listing.
    fn canonical_names(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        handle: &NodeHandle,
    ) -> Result<Vec<String>, Errno> {
        if let Some(names) = self.labelled(uid, caps, handle) {
            return Ok(names);
        }
        let groups = self.groups_of(uid)?;
        let below = self.with_live(handle, |fs, _| {
            let cred = credentials(uid, &groups, caps);
            DelegatedFs::bind(fs, &handle.projection)
                .names_down_to(&cred, handle.node.pinned())
                .map_err(VfsError::to_errno)
        })?;
        let mut names = handle
            .projection
            .path()
            .owned_components()
            .map_err(VfsError::to_errno)?;
        names
            .try_reserve(below.len())
            .map_err(|_| Errno::OutOfMemory)?;
        names.extend(below);
        Ok(names)
    }

    /// `handle`'s label as its names, when walking the label without
    /// following a link reaches the very node `handle` holds.
    fn labelled(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        handle: &NodeHandle,
    ) -> Option<Vec<String>> {
        let way = Way {
            at: At {
                start: Start::Root,
                path: &handle.opened_as,
            },
            posture: Posture {
                final_link: FinalLink::Follow,
                missing: MissingSteps::Refused,
                links: Links::Refuse,
            },
            spelling: Spelling::Unkept,
        };
        let reaches = self.resolved(
            uid,
            caps,
            way,
            MountUse::Read,
            |_, _, served, _, landing| {
                let reached = landing.walk.reached()?;
                Ok(reached.node == handle.node.pinned()
                    && *landing.projection == *handle.projection
                    && served.nodes.instance() == handle.node.instance())
            },
            |_, _, _| Ok(false),
        );
        if reaches != Ok(true) {
            return None;
        }
        Path::parse(&handle.opened_as)
            .ok()
            .map(Path::into_components)
    }

    /// Run `op` on the driver of `handle`'s volume, locked, once the node's
    /// projection is still in the mount table and its registration current:
    /// a descriptor of a volume detached, replaced or retracted answers
    /// [`Errno::Stale`], and never reaches whatever is attached there since.
    fn with_live<R>(
        &self,
        handle: &NodeHandle,
        op: impl FnOnce(&mut F, [u8; 16]) -> Result<R, Errno>,
    ) -> Result<R, Errno> {
        let (driver, volume) = self
            .mount
            .registered(handle.node.instance())
            .ok_or(Errno::Stale)?;
        let mut fs = driver.lock();
        if !handle.projection.is_live() || handle.node.volume().is_retired() {
            return Err(Errno::Stale);
        }
        op(&mut fs, volume)
    }

    /// As [`Self::with_live`], with the pinned node held for `op` and the
    /// credentials of `who` built from the identity table.
    fn with_node<R>(
        &self,
        handle: &NodeHandle,
        who: &Acting<'_>,
        op: impl FnOnce(HeldNode<'_, F>, &Credentials<'_>, [u8; 16]) -> Result<R, VfsError>,
    ) -> Result<R, Errno> {
        let groups = self.groups_of(who.uid)?;
        self.with_live(handle, |fs, volume| {
            let cred = credentials(who.uid, &groups, who.caps);
            let held = HeldNode::of(fs, handle.node.pinned(), &handle.projection)
                .map_err(VfsError::to_errno)?;
            op(held, &cred, volume).map_err(VfsError::to_errno)
        })
    }
}

impl<F> MountedFilesystemService<F>
where
    F: FilesystemRead
        + FilesystemWrite
        + FilesystemSecurity
        + FilesystemStats
        + FilesystemAttrsProvider
        + Send
        + 'static,
{
    /// As [`Self::with_node`], over the volume's attribute store, `op` told
    /// whether the owner may pass the mode: never through a grant. A volume
    /// whose format stores no attributes answers [`Errno::NotSupported`].
    fn with_attrs<R>(
        &self,
        handle: &NodeHandle,
        who: &Acting<'_>,
        op: impl for<'a> FnOnce(
            &mut HeldNode<'a, dyn FilesystemAttrsFs + 'a>,
            &Credentials<'_>,
            bool,
        ) -> Result<R, VfsError>,
    ) -> Result<R, Errno> {
        let groups = self.groups_of(who.uid)?;
        self.with_live(handle, |fs, _| {
            let Some(attrs) = fs.attrs_fs() else {
                return Err(Errno::NotSupported);
            };
            let cred = credentials(who.uid, &groups, who.caps);
            let mut held = HeldNode::of(attrs, handle.node.pinned(), &handle.projection)
                .map_err(VfsError::to_errno)?;
            op(&mut held, &cred, !who.granted).map_err(VfsError::to_errno)
        })
    }
}

/// What an operation resolves: a path and where it starts, how links and
/// missing names are treated, and what is kept of the path.
struct Way<'a> {
    at: At<'a>,
    posture: Posture,
    spelling: Spelling,
}

impl<'a> Way<'a> {
    /// `at`, a final link treated as `final_link`, nothing kept.
    fn to(at: At<'a>, final_link: FinalLink) -> Self {
        Self {
            at,
            posture: Posture::of(final_link),
            spelling: Spelling::Unkept,
        }
    }
}

/// The registered drivers, held one projection at a time.
struct Registry<'m, F: 'static> {
    mount: &'m LateFilesystem<F>,
}

/// What an operation on a registered volume learns of it beside its driver.
struct Served {
    /// The registration its pins are taken on.
    nodes: Arc<VolumeNodes>,
    /// Its published identity.
    volume_id: [u8; 16],
}

impl<F> Drivers for Registry<'_, F>
where
    F: FilesystemRead + FilesystemWrite + FilesystemSecurity + Send + 'static,
{
    type Fs = F;
    type Facts = Served;

    fn hold<T>(
        &mut self,
        serve: Serve<'_>,
        op: impl FnOnce(&mut Self::Fs, &Self::Facts) -> Result<T, VfsError>,
    ) -> Result<T, VfsError> {
        // A held node answers for its own registration, which only it may
        // reach; a walk that merely crossed into a projection that has since
        // gone finds nothing there.
        let (driver, served, gone) = if let Some(pin) = serve.pin {
            let (driver, volume_id) = self
                .mount
                .registered(pin.instance())
                .ok_or(VfsError::Stale)?;
            let nodes = Arc::clone(pin.volume());
            (driver, Served { nodes, volume_id }, VfsError::Stale)
        } else {
            let handle = serve.projection.backing().ok_or(VfsError::NotFound)?;
            let volume = self.mount.serving(handle).map_err(|_| VfsError::Unserved)?;
            let served = Served {
                nodes: volume.nodes,
                volume_id: volume.volume_id,
            };
            (volume.driver, served, VfsError::NotFound)
        };
        let mut fs = driver.lock();
        if !serve.projection.is_live() || served.nodes.is_retired() {
            return Err(gone);
        }
        op(&mut fs, &served)
    }

    fn keep(
        fs: &mut Self::Fs,
        served: &Self::Facts,
        node: NodeId,
        kind: DriverNodeKind,
    ) -> Result<Kept, VfsError> {
        let slot = served.nodes.admit()?;
        let pinned = fs.pin(node).map_err(map_driver_error)?;
        NodeRef::new(slot, pinned, kind).map(Kept::Pinned)
    }

    fn volume(served: &Self::Facts) -> [u8; 16] {
        served.volume_id
    }
}

/// A volume serving a projection: its driver, its registration, and its
/// published identity.
struct ServedVolume<F: ?Sized> {
    driver: Arc<DriverLock<F>>,
    nodes: Arc<VolumeNodes>,
    volume_id: [u8; 16],
}

/// What `uid`, in `groups` and holding `caps`, acts as.
fn credentials<'a>(uid: u32, groups: &'a Groups, caps: &'a dyn CapabilityQuery) -> Credentials<'a> {
    Credentials {
        uid: UserId(uid),
        gid: groups.primary,
        supplementary_gids: &groups.supplementary,
        caps,
    }
}

/// What an unlink under `flags` expects of the node its name holds.
///
/// # Errors
///
/// [`Errno::OutOfRange`] for an expectation the flags do not name, flags
/// naming one with none given, or an expectation of the node alone that
/// carries another field.
fn unlink_expecting(
    flags: UnlinkFlags,
    expect: Option<&NodeExpect>,
) -> Result<Option<Expect<'_>>, Errno> {
    let node = flags.contains(UnlinkFlags::EXPECT_NODE);
    let unchanged = flags.contains(UnlinkFlags::EXPECT_UNCHANGED);
    match (node, unchanged, expect) {
        (false, false, None) => Ok(None),
        (true, false, Some(expect)) => node_alone(expect).map(Some),
        (false, true, Some(expect)) => Ok(Some(Expect::Unchanged(expect))),
        _ => Err(Errno::OutOfRange),
    }
}

/// What a rename under `flags` expects of the node it moves.
///
/// # Errors
///
/// As [`unlink_expecting`].
fn rename_expecting(
    flags: RenameFlags,
    expect: Option<&NodeExpect>,
) -> Result<Option<Expect<'_>>, Errno> {
    match (flags.expects(), expect) {
        (false, None) => Ok(None),
        (true, Some(expect)) => node_alone(expect).map(Some),
        _ => Err(Errno::OutOfRange),
    }
}

/// The expectation of the node `expect` names, which asks nothing else of
/// it.
fn node_alone(expect: &NodeExpect) -> Result<Expect<'static>, Errno> {
    if expect.names_only_the_node() {
        Ok(Expect::Node(expect.id))
    } else {
        Err(Errno::OutOfRange)
    }
}

/// The label a node a walk ended on is opened as.
fn label(spelled: Option<Vec<String>>) -> Result<Arc<String>, VfsError> {
    let spelled = spelled.ok_or(VfsError::Io)?;
    Arc::try_new(spell(&spelled)?).map_err(|_| VfsError::OutOfMemory)
}

/// The record of the catalog a resolution ended at; a name in it no
/// projection is shown under answers `missing`.
fn catalog_record<'v>(
    vfs: &'v Vfs,
    end: &'v CatalogEnd,
    missing: VfsError,
) -> Result<&'v Metadata, VfsError> {
    if !end.absent.is_empty() {
        return Err(missing);
    }
    vfs.template_of(&end.catalog)
}

/// Whether `meta` lets `cred` have each access `want` asks for.
fn admits_each(meta: &Metadata, cred: &Credentials<'_>, want: Wanted) -> Result<(), VfsError> {
    Access::asked_by(want).try_for_each(|asked| meta.authorize(cred, asked))
}

/// Open the catalog a resolution ended at, which opens as the directory it
/// is: a name in it no projection is shown under opens nothing, and nothing
/// can be made in one.
fn open_catalog(end: CatalogEnd, flags: OpenFlags) -> Result<Opened, VfsError> {
    if !end.absent.is_empty() {
        return Err(if flags.contains(OpenFlags::CREATE) {
            VfsError::ReadOnly
        } else {
            VfsError::NotFound
        });
    }
    if flags.contains(OpenFlags::CREATE) && flags.contains(OpenFlags::EXCLUSIVE) {
        return Err(VfsError::AlreadyExists);
    }
    if !flags.contains(OpenFlags::DIRECTORY) && (flags.is_read() || flags.is_write()) {
        return Err(VfsError::IsADirectory);
    }
    Ok(Opened::Catalog(end.catalog))
}

/// What `stat` reports of the catalog a resolution ended at.
fn catalog_stat(vfs: &Vfs, end: &CatalogEnd) -> Result<FileStat, VfsError> {
    let meta = catalog_record(vfs, end, VfsError::NotFound)?;
    Ok(FileStat {
        kind: FileKind::Directory,
        nlink: NodeInfo::SINGLE_NAME,
        size: 0,
        allocated: 0,
        mode: u32::from(meta.mode.bits()),
        uid: meta.owner.0,
        gid: meta.group.0,
        id: FileId::NONE,
        times: NodeTimes::default(),
        content_gen: NodeInfo::NO_CONTENT_GEN,
    })
}

/// Where a directory's listing is reported: the volume its entries live on,
/// and, for a projection's floor, that projection's path, whose children the
/// listing shows.
#[derive(Copy, Clone)]
struct Shown<'a> {
    volume: [u8; 16],
    floor: Option<&'a Path>,
}

/// The listing of the directory `held`, as `shown`: its own entries, then,
/// for a projection's floor, the projections attached beneath it that the
/// volume holds no node of.
fn list_held<F: FilesystemRead + ?Sized>(
    vfs: &Vfs,
    held: &mut HeldNode<'_, F>,
    cred: &Credentials<'_>,
    owner_exempt: bool,
    shown: Shown<'_>,
    at: &mut Listing,
    each: &mut dyn FnMut(&DirEntry<'_>) -> DirVisit,
) -> Result<(), VfsError> {
    // Each entry's kind and sizes come from the listing driver itself, never
    // from a per-child path re-resolution: a child path can be covered by a
    // *different* mount, and re-resolving it here would judge it against the
    // wrong volume.
    if let Some(entries) = at.entries() {
        let end = held.list_from(cred, owner_exempt, entries, &mut |entry| {
            each(&wire_entry(shown.volume, entry))
        })?;
        if end == ListEnd::Stopped {
            return Ok(());
        }
        at.finish_entries();
    }
    // A floor no name reaches any more lists nothing, the projections that
    // were attached beneath its name included.
    let floor = match shown.floor {
        Some(floor) if !held.is_removed() => floor,
        _ => {
            at.finish();
            return Ok(());
        }
    };
    // A projection attached beneath is part of its parent's listing even when
    // the parent volume holds no node of that name. They follow the entries
    // in name order, so a batch resumes after the last one handed over, and
    // one the volume holds a node of was listed with the entries.
    let Some(after) = at.mounts().map(|after| after.clone()) else {
        return Ok(());
    };
    let mount_points = covered_after(vfs, floor, after.get())?;
    let mut names: Vec<&[u8]> = Vec::new();
    names
        .try_reserve_exact(mount_points.len())
        .map_err(|_| VfsError::OutOfMemory)?;
    names.extend(mount_points.iter().map(String::as_bytes));
    let held_names = held.lookup_entries(cred, owner_exempt, &names)?;
    for (name, found) in mount_points.iter().zip(held_names) {
        if found.is_some() {
            continue;
        }
        if each(&mount_point_wire(name)) == DirVisit::Stop {
            return Ok(());
        }
        if let Some(after) = at.mounts() {
            after.set(name.as_bytes())?;
        }
    }
    at.finish();
    Ok(())
}

/// What a listing of the directory `held`, as `shown`, reports for each of
/// `names`, merged, for a projection's floor, with the projections attached
/// beneath it.
fn looked_up<F: FilesystemRead + ?Sized>(
    vfs: &Vfs,
    held: &mut HeldNode<'_, F>,
    cred: &Credentials<'_>,
    owner_exempt: bool,
    shown: Shown<'_>,
    names: &[&[u8]],
) -> Result<LookedUp, VfsError> {
    let found = held.lookup_entries(cred, owner_exempt, names)?;
    let dir = node_identity(shown.volume, held.ino());
    let found = found
        .into_iter()
        .map(|entry| entry.map(|entry| readdir_entry(shown.volume, entry)));
    match shown.floor {
        Some(path) => with_mount_points(vfs, path, names, dir, found),
        None => Ok(LookedUp {
            dir,
            entries: collected(names.len(), found).ok_or(VfsError::OutOfMemory)?,
            children: ChildMounts::default(),
        }),
    }
}

/// The listing of the catalog at `point`, which the caller may read: the
/// volumes attached beneath it, in name order, resumed after the last one a
/// batch handed over.
fn list_catalog(
    vfs: &Vfs,
    point: &Path,
    at: &mut Listing,
    each: &mut dyn FnMut(&DirEntry<'_>) -> DirVisit,
) -> Result<(), VfsError> {
    if at.entries().is_some() {
        at.finish_entries();
    }
    let Some(after) = at.mounts().map(|after| after.clone()) else {
        return Ok(());
    };
    for name in covered_after(vfs, point, after.get())? {
        if each(&mount_point_wire(&name)) == DirVisit::Stop {
            return Ok(());
        }
        if let Some(after) = at.mounts() {
            after.set(name.as_bytes())?;
        }
    }
    at.finish();
    Ok(())
}

/// `found`, the entries a directory holds of `names`, with each name it
/// holds none of answered by a mount point of that name beneath `path`: the
/// same merge a listing makes.
fn with_mount_points(
    vfs: &Vfs,
    path: &Path,
    names: &[&[u8]],
    dir: FileId,
    found: impl Iterator<Item = Option<ReaddirEntry>>,
) -> Result<LookedUp, VfsError> {
    let mounts = vfs.mounts();
    let children = mounts.children_of(path);
    let mut covered: Vec<&String> = Vec::new();
    for name in mounts
        .direct_children(path)
        .filter_map(|mount| mount.path().components().last())
    {
        covered.try_reserve(1).map_err(|_| VfsError::OutOfMemory)?;
        covered.push(name);
    }
    let entries = collected(
        names.len(),
        found.zip(names).map(|(entry, &name)| {
            entry.or_else(|| {
                covered
                    .iter()
                    .find(|mounted| mounted.as_bytes() == name)
                    .map(|mounted| mount_point_entry(String::clone(mounted)))
            })
        }),
    )
    .ok_or(VfsError::OutOfMemory)?;
    Ok(LookedUp {
        dir,
        entries,
        children,
    })
}

/// A node's system-wide identity: its number as the format gives it
/// ([`NodeInfo::ino`]) paired with the id of the volume it lives on.
///
/// The one place that pair is assembled, so a `stat` and a listing of the
/// same node can never report different identities.
const fn node_identity(volume: [u8; 16], ino: u64) -> FileId {
    FileId { volume, node: ino }
}

/// What `stat` reports of a node of `volume` described by `info`, however it
/// was reached.
fn file_stat(info: &DelegatedInfo, volume: [u8; 16]) -> FileStat {
    FileStat {
        kind: file_kind(info.kind),
        nlink: info.nlink,
        size: info.size,
        allocated: info.allocated,
        mode: u32::from(info.meta.mode.bits()),
        uid: info.meta.owner.0,
        gid: info.meta.group.0,
        id: node_identity(volume, info.ino),
        times: info.times,
        content_gen: info.content_gen,
    }
}

/// The streamed record of a listed entry on `volume`.
fn wire_entry<'a>(volume: [u8; 16], entry: &DelegatedRef<'a>) -> DirEntry<'a> {
    DirEntry {
        kind: file_kind(entry.info.kind),
        size: entry.info.size,
        allocated: entry.info.allocated,
        // The readdir stream carries the cheap common-case modification
        // stamp; a consumer wanting other times stats the entry.
        modified: entry.info.times.modified,
        id: node_identity(volume, entry.info.ino),
        nlink: entry.info.nlink,
        content_gen: entry.info.content_gen,
        name: entry.name.as_bytes(),
    }
}

/// The owned `readdir` record of a listed entry on `volume`.
fn readdir_entry(volume: [u8; 16], mut entry: DelegatedEntry) -> ReaddirEntry {
    let name = core::mem::take(&mut entry.name);
    let listed = DelegatedRef {
        info: entry.info,
        name: "",
    };
    ReaddirEntry::named(&wire_entry(volume, &listed), name)
}

/// The streamed record of a covered mount point the parent volume holds no
/// node for: a directory by construction, with the placeholders a stampless
/// backing reports, since no identity, stamp or name count of its own is
/// reachable through the parent.
fn mount_point_wire(name: &str) -> DirEntry<'_> {
    DirEntry {
        kind: FileKind::Directory,
        size: 0,
        allocated: 0,
        modified: Time64::UNIX_EPOCH,
        id: FileId::NONE,
        nlink: NodeInfo::SINGLE_NAME,
        content_gen: NodeInfo::NO_CONTENT_GEN,
        name: name.as_bytes(),
    }
}

/// The owned record of a covered mount point.
fn mount_point_entry(name: String) -> ReaddirEntry {
    ReaddirEntry::named(&mount_point_wire(""), name)
}

/// The names of the mounts directly beneath `path` that sort after `after`,
/// in name order, copied out so no mount-table guard is held into the
/// driver: the table's lock prefers writers, so a second read under a held
/// guard deadlocks against a waiting mount.
fn covered_after(vfs: &Vfs, path: &Path, after: &[u8]) -> Result<Vec<String>, VfsError> {
    let mut names = Vec::new();
    for mount in vfs.mounts().direct_children(path) {
        let Some(name) = mount.path().components().last() else {
            continue;
        };
        if name.as_bytes() > after {
            names.try_reserve(1).map_err(|_| VfsError::OutOfMemory)?;
            let mut owned = String::new();
            owned
                .try_reserve_exact(name.len())
                .map_err(|_| VfsError::OutOfMemory)?;
            owned.push_str(name);
            names.push(owned);
        }
    }
    names.sort_unstable();
    Ok(names)
}

/// Flush `driver`'s buffered writes to its device. `abi-v1` has no dedicated
/// I/O errno, so a device fault collapses onto the code the VFS gives a driver
/// fault.
fn flush<F: FilesystemWrite>(driver: &DriverLock<F>) -> Result<(), Errno> {
    driver.lock().flush().map_err(|_| VfsError::Io.to_errno())
}

/// Map a driver structural node kind to the userland [`FileKind`] the
/// `fs_*` contract exposes.
fn file_kind(kind: DriverNodeKind) -> FileKind {
    match kind {
        DriverNodeKind::Directory => FileKind::Directory,
        DriverNodeKind::RegularFile => FileKind::Regular,
        DriverNodeKind::Symlink => FileKind::Symlink,
    }
}

impl<F> FilesystemService for MountedFilesystemService<F>
where
    F: FilesystemRead
        + FilesystemWrite
        + FilesystemSecurity
        + FilesystemStats
        + FilesystemAttrsProvider
        + Send
        + 'static,
{
    fn open(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        flags: OpenFlags,
        mode: CreateMode,
    ) -> Result<Opened, Errno> {
        let purpose = if flags.is_write() {
            MountUse::Write
        } else {
            MountUse::Read
        };
        let way = Way {
            at,
            posture: Posture::of(FinalLink::for_open(flags)),
            spelling: Spelling::Label,
        };
        self.resolved(
            uid,
            caps,
            way,
            purpose,
            |_, fs, served, cred, landing| {
                let Landing {
                    projection,
                    walk,
                    spelled,
                } = landing;
                let (node, at_floor) = DelegatedFs::bind(fs, &projection).open_at(
                    cred,
                    walk,
                    flags,
                    &served.nodes,
                    mode,
                )?;
                Ok(Opened::Node {
                    kind: node.kind(),
                    node,
                    opened_as: label(spelled)?,
                    projection,
                    at_floor,
                })
            },
            |_, _, end| open_catalog(end, flags),
        )
    }

    fn read_program(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, Errno> {
        self.resolved(
            uid,
            caps,
            Way::to(at, FinalLink::Follow),
            MountUse::Run,
            |_, fs, _, cred, landing| {
                let mut delegated = DelegatedFs::bind(fs, &landing.projection);
                delegated
                    .held(landing.walk)?
                    .0
                    .read(cred, false, offset, buf)
            },
            |vfs, _, end| {
                catalog_record(vfs, &end, VfsError::NotFound)?;
                Err(VfsError::IsADirectory)
            },
        )
    }

    fn readdir_bounded(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        final_link: FinalLink,
        limit: usize,
    ) -> Result<Vec<ReaddirEntry>, Errno> {
        let mut entries = Vec::new();
        let mut failed = None;
        let mut each = |entry: &DirEntry<'_>| {
            if entries.len() == limit {
                failed = Some(Errno::LimitExceeded);
                return DirVisit::Stop;
            }
            match entries
                .try_reserve(1)
                .map_err(|_| Errno::OutOfMemory)
                .and_then(|()| ReaddirEntry::from_wire(entry))
            {
                Ok(owned) => {
                    entries.push(owned);
                    DirVisit::Take
                }
                Err(err) => {
                    failed = Some(err);
                    DirVisit::Stop
                }
            }
        };
        let mut at_entry = Listing::default();
        // The catalog is listed once the walk has let its driver go.
        let catalog = self.resolved(
            uid,
            caps,
            Way::to(at, final_link),
            MountUse::Read,
            |vfs, fs, served, cred, landing| {
                let mut delegated = DelegatedFs::bind(fs, &landing.projection);
                let (mut held, at_floor) = delegated.held(landing.walk)?;
                let shown = Shown {
                    volume: served.volume_id,
                    floor: at_floor.then_some(landing.projection.path()),
                };
                list_held(vfs, &mut held, cred, false, shown, &mut at_entry, &mut each)?;
                Ok(None)
            },
            |vfs, cred, end| {
                catalog_record(vfs, &end, VfsError::NotFound)?.authorize(cred, Access::Read)?;
                Ok(Some(end.catalog))
            },
        )?;
        if let Some(catalog) = catalog {
            let vfs = self.mount.vfs()?;
            list_catalog(vfs, catalog.path(), &mut at_entry, &mut each)
                .map_err(VfsError::to_errno)?;
        }
        failed.map_or(Ok(entries), Err)
    }

    fn lookup_entries(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        final_link: FinalLink,
        names: &[&[u8]],
    ) -> Result<LookedUp, Errno> {
        self.resolved(
            uid,
            caps,
            Way::to(at, final_link),
            MountUse::Read,
            |vfs, fs, served, cred, landing| {
                let mut delegated = DelegatedFs::bind(fs, &landing.projection);
                let (mut held, at_floor) = delegated.held(landing.walk)?;
                let shown = Shown {
                    volume: served.volume_id,
                    floor: at_floor.then_some(landing.projection.path()),
                };
                looked_up(vfs, &mut held, cred, false, shown, names)
            },
            |vfs, cred, end| {
                catalog_record(vfs, &end, VfsError::NotFound)?.authorize(cred, Access::Read)?;
                let path = end.catalog.path();
                with_mount_points(vfs, path, names, FileId::NONE, names.iter().map(|_| None))
            },
        )
    }

    fn mount_epoch(&self) -> u64 {
        self.mount.vfs().map_or(0, |vfs| vfs.mounts().epoch())
    }

    fn stat(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        final_link: FinalLink,
    ) -> Result<FileStat, Errno> {
        self.resolved(
            uid,
            caps,
            Way::to(at, final_link),
            MountUse::Read,
            |_, _, served, _, landing| {
                let reached = landing.walk.reached()?;
                let info = DelegatedInfo::of(&reached.info, reached.meta);
                Ok(file_stat(&info, served.volume_id))
            },
            |vfs, _, end| catalog_stat(vfs, &end),
        )
    }

    fn sync_all(&self, _uid: u32, _caps: &dyn CapabilityQuery) -> Result<(), Errno> {
        self.mount.vfs()?;
        // Every volume is flushed whatever another answered; the first refusal
        // is the one reported.
        let mut outcome = Ok(());
        for driver in self.mount.all_drivers() {
            outcome = outcome.and(flush(&driver));
        }
        outcome
    }

    fn mkdir(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        mode: CreateMode,
    ) -> Result<(), Errno> {
        self.resolved(
            uid,
            caps,
            Way::to(at, FinalLink::Keep),
            MountUse::Write,
            |_, fs, _, cred, landing| {
                DelegatedFs::bind(fs, &landing.projection).create_at(
                    cred,
                    landing.walk,
                    DriverNodeKind::Directory,
                    mode,
                )
            },
            |_, _, _| Err(VfsError::ReadOnly),
        )
    }

    fn unlink(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        flags: UnlinkFlags,
        expect: Option<&NodeExpect>,
    ) -> Result<(), Errno> {
        let expect = unlink_expecting(flags, expect)?;
        self.resolved(
            uid,
            caps,
            Way::to(at, FinalLink::Keep),
            MountUse::Write,
            |_, fs, served, cred, landing| {
                DelegatedFs::bind(fs, &landing.projection).remove_at(
                    cred,
                    landing.walk,
                    flags.is_directory_only(),
                    expect.map(|expect| expect.on(served.volume_id)),
                )
            },
            |_, _, _| Err(VfsError::ReadOnly),
        )
    }

    fn symlink(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        target: &str,
        at: At<'_>,
    ) -> Result<(), Errno> {
        // Caller-supplied bytes, checked before anything is looked at.
        parse_link_target(target).map_err(VfsError::to_errno)?;
        self.resolved(
            uid,
            caps,
            Way::to(at, FinalLink::Keep),
            MountUse::Write,
            |_, fs, _, cred, landing| {
                DelegatedFs::bind(fs, &landing.projection).create_link_at(
                    cred,
                    landing.walk,
                    target,
                )
            },
            |_, _, _| Err(VfsError::ReadOnly),
        )
    }

    fn readlink(&self, uid: u32, caps: &dyn CapabilityQuery, at: At<'_>) -> Result<String, Errno> {
        self.resolved(
            uid,
            caps,
            Way::to(at, FinalLink::Keep),
            MountUse::Read,
            |_, fs, _, _, landing| {
                DelegatedFs::bind(fs, &landing.projection).read_link_at(landing.walk)
            },
            |vfs, _, end| {
                catalog_record(vfs, &end, VfsError::NotFound)?;
                Err(VfsError::InvalidPath)
            },
        )
    }

    fn realpath(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        mode: RealpathMode,
    ) -> Result<String, Errno> {
        // A path from a held directory is spelled from where that directory
        // stands now, not where it was opened.
        let start = match at.start {
            Start::Dir(handle) if tairix_path::is_relative_spelling(at.path) => {
                self.canonical_names(uid, caps, handle)?
            }
            Start::Root | Start::Dir(_) | Start::Catalog(_) => Vec::new(),
        };
        let way = Way {
            at,
            posture: canonicalising(mode),
            spelling: Spelling::Exact(start),
        };
        self.resolved(
            uid,
            caps,
            way,
            MountUse::Read,
            |_, _, _, _, landing| canonical_answer(vacancy(&landing.walk), mode, landing.spelled),
            |_, _, end| canonical_answer(end.absent.len(), mode, end.spelled),
        )
    }

    fn rename(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        src: At<'_>,
        dst: At<'_>,
        flags: RenameFlags,
        expect: Option<&NodeExpect>,
    ) -> Result<(), Errno> {
        let expect = rename_expecting(flags, expect)?;
        let (vfs, groups) = self.acting_in(uid)?;
        let cred = credentials(uid, &groups, caps);
        let keep = Posture::of(FinalLink::Keep);
        let from = Resolution::new(vfs, src, keep, Spelling::Unkept).map_err(VfsError::to_errno)?;
        let to = Resolution::new(vfs, dst, keep, Spelling::Unkept).map_err(VfsError::to_errno)?;
        let mut drivers = Registry { mount: self.mount };
        vfs.rename_between(&cred, &mut drivers, from, to, flags, expect)
            .map_err(VfsError::to_errno)
    }

    fn link(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        existing: At<'_>,
        new: At<'_>,
        existing_link: FinalLink,
    ) -> Result<(), Errno> {
        let (vfs, groups) = self.acting_in(uid)?;
        let cred = credentials(uid, &groups, caps);
        let existing = Resolution::new(vfs, existing, Posture::of(existing_link), Spelling::Unkept)
            .map_err(VfsError::to_errno)?;
        let new = Resolution::new(vfs, new, Posture::of(FinalLink::Keep), Spelling::Unkept)
            .map_err(VfsError::to_errno)?;
        let mut drivers = Registry { mount: self.mount };
        vfs.link_between(&cred, &mut drivers, existing, new)
            .map_err(VfsError::to_errno)
    }

    fn set_mode(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        mode: u32,
        final_link: FinalLink,
    ) -> Result<(), Errno> {
        // Defence in depth behind the dispatcher's own mask check, so no
        // in-kernel caller can write a corrupt record through this seam.
        if mode & !FS_MODE_MASK != 0 {
            return Err(Errno::OutOfRange);
        }
        self.resolved(
            uid,
            caps,
            Way::to(at, final_link),
            MountUse::Write,
            |_, fs, _, cred, landing| {
                let mut delegated = DelegatedFs::bind(fs, &landing.projection);
                delegated.held(landing.walk)?.0.set_mode(cred, false, mode)
            },
            |_, _, _| Err(VfsError::ReadOnly),
        )
    }

    fn set_owner(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        owner: u32,
        group: u32,
        final_link: FinalLink,
    ) -> Result<(), Errno> {
        self.resolved(
            uid,
            caps,
            Way::to(at, final_link),
            MountUse::Write,
            |_, fs, _, cred, landing| {
                let mut delegated = DelegatedFs::bind(fs, &landing.projection);
                delegated
                    .held(landing.walk)?
                    .0
                    .set_owner(cred, false, owner, group)
            },
            |_, _, _| Err(VfsError::ReadOnly),
        )
    }

    fn access(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        at: At<'_>,
        want: Wanted,
        final_link: FinalLink,
    ) -> Result<(), Errno> {
        self.resolved(
            uid,
            caps,
            Way::to(at, final_link),
            MountUse::Read,
            |_, _, _, cred, landing| {
                let reached = landing.walk.reached()?;
                admits_each(&reached.meta, cred, want)?;
                landing.projection.admits(reached.info.kind, want)
            },
            |vfs, cred, end| {
                admits_each(catalog_record(vfs, &end, VfsError::NotFound)?, cred, want)?;
                end.catalog.admits(DriverNodeKind::Directory, want)
            },
        )
    }

    fn chdir(&self, uid: u32, caps: &dyn CapabilityQuery, at: At<'_>) -> Result<Cwd, Errno> {
        let way = Way {
            at,
            posture: Posture::of(FinalLink::Follow),
            spelling: Spelling::Label,
        };
        self.resolved(
            uid,
            caps,
            way,
            MountUse::Read,
            |_, fs, served, cred, landing| {
                let Landing {
                    projection,
                    walk,
                    spelled,
                } = landing;
                // The view root is `Cwd::Root` whatever backs it, so it pins
                // nothing.
                if projection.path().is_root()
                    && matches!(walk, Walk::Bottom { at_floor: true, .. })
                {
                    walk.reached()?.meta.authorize(cred, Access::Execute)?;
                    return Ok(Cwd::Root);
                }
                let (node, at_floor) =
                    DelegatedFs::bind(fs, &projection).chdir_at(cred, walk, &served.nodes)?;
                Ok(Cwd::Dir(NodeHandle {
                    node,
                    opened_as: label(spelled)?,
                    projection,
                    authority: Authority::Holder,
                    at_floor,
                }))
            },
            |vfs, cred, end| {
                catalog_record(vfs, &end, VfsError::NotFound)?.authorize(cred, Access::Execute)?;
                Ok(if end.catalog.path().is_root() {
                    Cwd::Root
                } else {
                    Cwd::Catalog(end.catalog)
                })
            },
        )
    }

    fn canonical_of(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        start: Start<'_>,
    ) -> Result<String, Errno> {
        match start {
            Start::Root => spell(&[]).map_err(VfsError::to_errno),
            Start::Catalog(catalog) => {
                if self.mount.vfs()?.mounts().live(catalog).is_none() {
                    return Err(Errno::Stale);
                }
                spell(catalog.path().components()).map_err(VfsError::to_errno)
            }
            Start::Dir(handle) => {
                let names = self.canonical_names(uid, caps, handle)?;
                spell_canonical(&names).map_err(VfsError::to_errno)
            }
        }
    }

    fn read_node(
        &self,
        node: &NodeHandle,
        who: &Acting<'_>,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, Errno> {
        self.with_node(node, who, |mut held, cred, _| {
            held.read(cred, !who.granted, offset, buf)
        })
    }

    fn write_node(
        &self,
        node: &NodeHandle,
        who: &Acting<'_>,
        offset: u64,
        append: bool,
        data: &[u8],
    ) -> Result<usize, Errno> {
        self.with_node(node, who, |mut held, cred, _| {
            held.write(cred, !who.granted, offset, append, data)
        })
    }

    fn truncate_node(&self, node: &NodeHandle, who: &Acting<'_>, size: u64) -> Result<(), Errno> {
        self.with_node(node, who, |mut held, cred, _| {
            held.truncate(cred, !who.granted, size)
        })
    }

    fn punch_node(
        &self,
        node: &NodeHandle,
        who: &Acting<'_>,
        offset: u64,
        len: u64,
    ) -> Result<(), Errno> {
        self.with_node(node, who, |mut held, cred, _| {
            held.punch(cred, !who.granted, offset, len)
        })
    }

    fn seek_region_node(
        &self,
        node: &NodeHandle,
        who: &Acting<'_>,
        offset: u64,
        region: SeekRegion,
    ) -> Result<u64, Errno> {
        self.with_node(node, who, |mut held, cred, _| {
            held.seek_region(cred, !who.granted, offset, region)?
                .ok_or(VfsError::NoData)
        })
    }

    fn stat_node(&self, node: &NodeHandle, who: &Acting<'_>) -> Result<FileStat, Errno> {
        self.with_node(node, who, |held, _, volume| {
            Ok(file_stat(&held.into_info(), volume))
        })
    }

    fn readdir_node(
        &self,
        node: &NodeHandle,
        who: &Acting<'_>,
        at: &mut Listing,
        each: &mut dyn FnMut(&DirEntry<'_>) -> DirVisit,
    ) -> Result<(), Errno> {
        let vfs = self.mount.vfs()?;
        let floor = node.at_floor.then(|| node.projection.path());
        self.with_node(node, who, |mut held, cred, volume| {
            let shown = Shown { volume, floor };
            list_held(vfs, &mut held, cred, !who.granted, shown, at, each)
        })
    }

    fn lookup_entries_node(
        &self,
        node: &NodeHandle,
        who: &Acting<'_>,
        names: &[&[u8]],
    ) -> Result<LookedUp, Errno> {
        let vfs = self.mount.vfs()?;
        let floor = node.at_floor.then(|| node.projection.path());
        self.with_node(node, who, |mut held, cred, volume| {
            let shown = Shown { volume, floor };
            looked_up(vfs, &mut held, cred, !who.granted, shown, names)
        })
    }

    fn sync_node(&self, node: &NodeHandle, _who: &Acting<'_>) -> Result<(), Errno> {
        self.with_live(node, |fs, _| {
            fs.flush().map_err(|_| VfsError::Io.to_errno())
        })
    }

    fn set_mode_node(&self, node: &NodeHandle, who: &Acting<'_>, mode: u32) -> Result<(), Errno> {
        if mode & !FS_MODE_MASK != 0 {
            return Err(Errno::OutOfRange);
        }
        self.with_node(node, who, |mut held, cred, _| {
            held.set_mode(cred, who.granted, mode)
        })
    }

    fn set_owner_node(
        &self,
        node: &NodeHandle,
        who: &Acting<'_>,
        owner: u32,
        group: u32,
    ) -> Result<(), Errno> {
        self.with_node(node, who, |mut held, cred, _| {
            held.set_owner(cred, who.granted, owner, group)
        })
    }

    fn attr_get_node(
        &self,
        node: &NodeHandle,
        who: &Acting<'_>,
        key: &[u8],
        value_out: &mut [u8],
    ) -> Result<usize, Errno> {
        self.with_attrs(node, who, |held, cred, owner_exempt| {
            held.get_attr(cred, owner_exempt, key, value_out)
        })
    }

    fn attr_set_node(
        &self,
        node: &NodeHandle,
        who: &Acting<'_>,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), Errno> {
        self.with_attrs(node, who, |held, cred, owner_exempt| {
            held.set_attr(cred, owner_exempt, key, value)
        })
    }

    fn attr_list_node(
        &self,
        node: &NodeHandle,
        who: &Acting<'_>,
        index: u64,
        key_out: &mut [u8],
    ) -> Result<Option<usize>, Errno> {
        self.with_attrs(node, who, |held, cred, owner_exempt| {
            held.list_attr(cred, owner_exempt, index, key_out)
        })
    }

    fn attr_remove_node(
        &self,
        node: &NodeHandle,
        who: &Acting<'_>,
        key: &[u8],
    ) -> Result<(), Errno> {
        self.with_attrs(node, who, |held, cred, owner_exempt| {
            held.remove_attr(cred, owner_exempt, key)
        })
    }

    fn readdir_catalog(
        &self,
        uid: u32,
        caps: &dyn CapabilityQuery,
        catalog: &Projection,
        at: &mut Listing,
        each: &mut dyn FnMut(&DirEntry<'_>) -> DirVisit,
    ) -> Result<(), Errno> {
        if !catalog.is_live() {
            return Err(Errno::Stale);
        }
        let (vfs, groups) = self.acting_in(uid)?;
        let cred = credentials(uid, &groups, caps);
        vfs.template_of(catalog)
            .and_then(|meta| meta.authorize(&cred, Access::Read))
            .and_then(|()| list_catalog(vfs, catalog.path(), at, each))
            .map_err(VfsError::to_errno)
    }

    fn reap(&self, volume: &VolumeNodes) {
        self.mount.reap(volume);
    }

    fn watch_table(&self, node: &NodeHandle) -> Option<Arc<crate::fswatch::VolumeWatch>> {
        self.mount.watch_table(node.node.instance())
    }

    fn access_node(
        &self,
        node: &NodeHandle,
        who: &Acting<'_>,
        want: Access,
        judged: Judged,
    ) -> Result<(), Errno> {
        let owner_exempt = judged == Judged::AsOperation && !who.granted;
        self.with_node(node, who, |held, cred, _| {
            held.admits(cred, want, owner_exempt)
        })
    }

    fn mount_snapshot(&self) -> Vec<MountRecord> {
        // Before any volume is online there is no VFS; report "no mounts"
        // truthfully rather than fabricating one (fail closed).
        let Ok(vfs) = self.mount.vfs() else {
            return Vec::new();
        };
        // Copied out, so the table's guard is held across no driver's lock.
        let volumes = vfs.mounts().volumes();
        let online = self.mount.entries();
        volumes
            .into_iter()
            .filter_map(|(attachment, shown_at)| {
                // A volume attached in the namespace whose driver is not yet
                // registered serves nothing, so it is not reported.
                let entry = online_entry(&online, attachment.driver())?;
                // A volume in the middle of a long operation reports the
                // figures it last gave rather than holding the listing — and
                // everyone waiting on it — until it is done. A driver fault
                // reading its accounting reports zero usage: the volume
                // exists, only its figures are unknown.
                let usage = match entry.driver.try_lock() {
                    Some(mut driver) => {
                        let fresh = driver.stats().unwrap_or_default();
                        *entry.usage.lock() = fresh;
                        fresh
                    }
                    None => *entry.usage.lock(),
                };
                let target = match shown_at {
                    Some(at) => spell(at.components()).ok()?,
                    None => String::new(),
                };
                // `MountRecord::new` fails only on an over-long field or an
                // inconsistent usage report; such a volume is left out rather
                // than misreported.
                MountRecord::new(
                    entry.source.as_bytes(),
                    target.as_bytes(),
                    entry.fstype.as_bytes(),
                    attachment.flags(),
                    MountVolumeState {
                        usage,
                        availability: entry.availability,
                        medium: attachment.medium(),
                    },
                    entry.volume_id,
                )
                .ok()
            })
            .collect()
    }

    fn view_snapshot(&self) -> Vec<ViewBindingRecord> {
        let Ok(vfs) = self.mount.vfs() else {
            return Vec::new();
        };
        let online = self.mount.entries();
        let mounts = vfs.mounts();
        mounts
            .iter()
            .filter_map(|mount| {
                // A policy-only projection — the `/Storage` catalog — shows no
                // volume and is listed as such, so a path beneath it is never
                // credited to the volume above. A backed one is listed once its
                // volume is.
                let (subtree, volume_id) = match mount.backing() {
                    None => (String::new(), [0u8; 16]),
                    Some(driver) => (
                        spell(mount.backing_subtree()).ok()?,
                        online_entry(&online, driver)?.volume_id,
                    ),
                };
                ViewBindingRecord::new(
                    spell(mount.path().components()).ok()?.as_bytes(),
                    subtree.as_bytes(),
                    mount.flags(),
                    volume_id,
                )
                .ok()
            })
            .collect()
    }

    fn volume_io_health_snapshot(&self) -> Vec<VolumeIoHealthRecord> {
        // Served straight from the driver registry, which holds each volume's
        // block I/O source; a system with no mount table registers no driver
        // and so truthfully reports no volumes.
        self.mount.volume_io_health_records()
    }

    fn volume_io_stats_snapshot(&self) -> Vec<VolumeIoStatsRecord> {
        self.mount.volume_io_stats_records()
    }

    fn volume_io_queue_snapshot(&self) -> Vec<VolumeIoQueueRecord> {
        self.mount.volume_io_queue_records()
    }

    fn remove_if_endpoints_idle(
        &self,
        endpoints: &[u64],
        remove: &mut dyn FnMut() -> Result<Vec<u32>, Errno>,
    ) -> Result<Vec<u32>, Errno> {
        // The registry owns the volume<->endpoint mapping, and an attach
        // registers under the same lock this holds across the removal, so the
        // busy check and the removal are atomic against a concurrent attach.
        self.mount.remove_if_endpoints_idle(endpoints, remove)
    }
}

/// The registered volume `driver` names, in `online` as
/// [`LateFilesystem::entries`] sorted it.
fn online_entry<F>(online: &[SnapshotEntry<F>], driver: DriverHandle) -> Option<&SnapshotEntry<F>> {
    online
        .binary_search_by_key(&driver.as_u64(), |entry| entry.handle)
        .ok()
        .map(|at| &online[at])
}

#[cfg(test)]
#[path = "mounted_tests.rs"]
mod tests;
