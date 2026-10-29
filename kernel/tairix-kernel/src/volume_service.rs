//! The runtime volume attach/detach service (`plans/DEVICES.md` D3b,
//! `plans/FSSERVE.md` FSS5).
//!
//! The production [`VolumeService`] behind the `volume_attach` /
//! `volume_detach` syscalls: it connects the kernel blkio client to the
//! block-service endpoint + shared window a user-space block driver serves
//! (the USB mass-storage per-LUN nodes), windows the probed partition
//! extent, and publishes a volume node its format's user-space filesystem
//! driver binds. When that driver serves the volume (`volume_serve`), the
//! kernel's proxy for it is mounted under its `/Storage/<name>` catalog view
//! location with the removable-media flags, and the volume's stable identity
//! is published into the volume forest so `id::<volume-id>/…` paths resolve.
//! Detach reverses it: flush, unmount, unregister, unpublish — failing closed
//! (the volume stays attached) rather than discarding uncommitted data or a
//! node a descriptor holds.
//!
//! The syscall layer has already verified `CAP_FS_MOUNT` and the caller's
//! endpoint/window resource grants; this service re-validates everything
//! against live state and audits every attach/detach decision with a
//! stable event id (the drives.md `fs.hotplug.root_{added,removed}`
//! events; root publication itself flows through the shared
//! `publish_volume_identity` definition in `crate::system_mount`).
//!
//! An attach, recovery, serve or detach parks on device I/O or orders against
//! one that does, so each runs under its *device's* own sleeping lock
//! (`DeviceSlot`): the volumes of one disk
//! take its one client in turn and never interleave a half-built volume,
//! while a disk that stops answering holds up only the volumes that live on
//! it. What concerns every device is settled atomically where it lives — a
//! catalog name in the mount table, an identity in the volume forest, and a
//! volume another operation is building, recovering or retracting by that
//! operation's claim on its registry entry — never by one device's lock
//! held over another's. The attached-volume registry itself lives behind a
//! spin lock so the surprise-removal path can walk it from the
//! endpoint-teardown context without parking.
//!
//! # Surprise removal (`plans/DEVICES.md` D4)
//!
//! Every attach threads the device through a
//! [`JournaledBlock`], so the volume carries a bounded [`RetainedWrites`]
//! journal of the writes the device has accepted since its last committed
//! flush. The service implements
//! [`tairix_kernel_core::callreg::EndpointVanishObserver`]: when a
//! volume's serving block driver dies (the stick was yanked and the HCD
//! retracted its nodes), the vanished endpoint drives the transition —
//!
//! * **clean** (nothing uncommitted): the volume is simply retracted —
//!   unmounted, unregistered, unpublished — with one audit event; no
//!   drama (drives.md §10). A descriptor held across it answers stale.
//! * **dirty**: the volume enters *unavailable-dirty* — the root, alias,
//!   and mount stay visible but every operation, a held descriptor's
//!   included, fails closed with a typed device fault (the dead endpoint
//!   guarantees it), the retained write set is kept for the verified
//!   re-insert replay or an explicit force-discard, and the event records
//!   how many bytes are held.
//! * **lost** (retention was abandoned under the budget/pressure gate, or
//!   a failed write left the medium state unknown): *unavailable-lost*,
//!   and the event says so — uncommitted data existed that is not held.
//!
//! A plain `volume_detach` of an unavailable volume is refused: discarding
//! the retained set is the deliberate, separately-audited **force-unmount**
//! (`plans/DEVICES.md` D4b), never an implicit side effect. A force detach
//! (`VolumeDetachRequest::force`) still commits a healthy volume cleanly
//! when it can; only when nothing can be committed — the volume is
//! unavailable, or its flush fails — does it discard the retained set,
//! logging the deliberate data loss with its own event id.
//!
//! A plain detach is also refused while a descriptor holds one of the
//! volume's nodes, or a bind stands within the volume. A force detach revokes
//! the descriptors — the driver completing each deletion they deferred — so a
//! revoked descriptor answers stale, and withdraws the binds within the
//! volume first, as a medium that leaves does.
//!
//! # Verified re-insert (`plans/DEVICES.md` D4c)
//!
//! An attach whose probed identity (`lib/fsprobe`) matches an unavailable
//! volume is a **re-insert** and is recovered in place rather than
//! re-attached as a duplicate. The journal carries a dual-acceptance
//! shadow of the volume's mutation-evidence window (the region any
//! foreign mutation must rewrite: the `ARXFS` superblock ring, the ext4
//! superblock, the FAT32 boot+`FSInfo` head — honestly weaker for weaker
//! formats), seeded at attach and maintained on every write. When the
//! re-read window proves non-mutation — every evidence block equals its
//! last-committed or latest shadow copy — the retained writes are
//! replayed, the device cache committed, and the volume returns to full
//! service under its original mount, name, and published root. Any doubt
//! (foreign mutation, a moved extent, abandoned retention, a replay or
//! flush failure) fails closed: the volume returns **read-only** in the
//! *recovery-conflict* state with the retained set still held for
//! explicit salvage or the audited force-discard — never a silent merge.
//! Either way the volume returns under a fresh registration, so a
//! descriptor held across the removal answers stale.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_abi::blkio::BlkMedium;
use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{
    DirEntry, DirVisit, FilesystemAttrs, FilesystemAttrsFs, FilesystemAttrsProvider,
    FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite, MountFlags, NameMatching,
    NodeId, NodeIdentity, NodeInfo, NodeKind, NodeSecurity, Unpinned, VolumeStats, WriteRefusal,
};
use tairix_abi::fsserve::{is_served_endpoint, ServeOffer};
use tairix_abi::hwtree::HwDeviceClass;
use tairix_abi::netattach::{AttachReply, FsserveCtl, NetworkAttach, NetworkOffer, OfferId};
use tairix_abi::sysinfo::MountAvailability;
use tairix_abi::volume::{
    FormatName, VolumeAttachRequest, VolumeCtl, VolumeDetachRequest, VolumeFsType, VolumeInfo,
};
use tairix_abi::{DriverError, DriverHandle, Errno, HwMatchKey, HwNode, HwResource, SeekRegion};
use tairix_bytedev::ByteDevice;
use tairix_fsprobe::{Evidence, ProbedVolume, Recognised};
use tairix_kernel_core::callreg::EndpointVanishObserver;
use tairix_kernel_core::devres::installed_shared_mem_facility;
use tairix_kernel_core::fs::blkclient::BlkClient;
use tairix_kernel_core::fs::blkmeter::VolumeIoSource;
use tairix_kernel_core::fs::served::ServedFs;
use tairix_kernel_core::fs::{
    ChunkSink, ChunkSource, JournaledBlock, OfferingServer, Principal, RetainedWrites,
    ServeChannel, VolumeNodes,
};
use tairix_kernel_core::fswatch::{ClaimRef, WatchRegistry};
use tairix_kernel_core::sharedreg::kernel_hold;
use tairix_kernel_core::{
    Attachment, BindsWithin, DriverLock, Metadata, Mode, Path, SleepLock, Vfs, VfsError,
    VolumePublishError, VolumeService,
};
use tairix_kernel_ipc::EndpointId;
use tairix_kernel_sec::{GroupId, UserId};
use tairix_log::{log, Event, EventId, Field, FieldValue, Level, Sink};
use tairix_partition::PartitionBlock;
use tairix_reclaim::{CacheBudget, MemoryPressure};
use tairix_sync::{OnceCell, SpinLock};
use tairix_util::fallible::concatenated;

use crate::block_cache::{BlockCache, REMOVABLE_DISK_OWNER};
use crate::hwtree_store::HwTreeStore;
use crate::kernel_fs::KernelFs;
use crate::served_volume::ServedVolume;
use crate::shared_block::{OwnedBlockWindow, SharedBlock};
use crate::system_mount::{cached, publish_volume_identity, LATE_FILESYSTEM, VOLUME_FOREST};
use crate::volume_policy::{Presented, PresentedFs, LATE_STORAGE_GID};

#[path = "network_volume.rs"]
mod network;

/// Audit event: a runtime volume was first served by its driver, mounted,
/// and its root published (drives.md `fs.hotplug.root_added`).
const VOLUME_ATTACHED: EventId = EventId(4172);

/// Audit event: a runtime volume was attached read-only because its
/// filesystem driver refuses to write it. The `cause` field names why — a
/// feature it does not maintain, errors a checker recorded, a crash that
/// left the volume for a checker — so a user told a write failed learns
/// what to do about it.
const VOLUME_ATTACHED_READ_ONLY: EventId = EventId(4180);

/// Audit event: a runtime volume's requested catalog name is already taken.
/// Not a refusal of the volume: the volume manager tries its next candidate
/// name on this answer, so it is recorded at `Info`.
const VOLUME_NAME_TAKEN: EventId = EventId(4181);

/// Audit event: a runtime volume attach was refused; nothing was mounted
/// (drives.md `fs.hotplug.root_added` deny half). The `cause` field names
/// the check that declined, secret-free.
const VOLUME_ATTACH_REFUSED: EventId = EventId(4173);

/// Audit event: a runtime volume was flushed, unmounted, and its root
/// withdrawn (drives.md `fs.hotplug.root_removed`).
const VOLUME_DETACHED: EventId = EventId(4174);

/// Audit event: a runtime volume detach was refused; the volume stays
/// attached and no data was discarded.
const VOLUME_DETACH_REFUSED: EventId = EventId(4175);

/// What a [`VOLUME_DETACH_REFUSED`] record says, whatever refused the detach.
const DETACH_REFUSED: &str = "volume-service: runtime volume detach refused";

/// Audit event: a volume's serving driver vanished with nothing
/// uncommitted; the volume was retracted cleanly (drives.md §10 — a
/// clean surprise removal is no drama).
const VOLUME_SURPRISE_REMOVED_CLEAN: EventId = EventId(4176);

/// Audit event: a volume's serving driver vanished with uncommitted
/// writes; the volume is unavailable-dirty and the retained set is held
/// for verified re-insert or an explicit force-discard. Carries the
/// retained byte count.
const VOLUME_SURPRISE_REMOVED_DIRTY: EventId = EventId(4177);

/// Audit event: a volume's serving driver vanished after retention was
/// abandoned (budget/pressure refusal or a failed write): uncommitted
/// data existed that is **not** held. The volume is unavailable-lost.
const VOLUME_SURPRISE_REMOVED_LOST: EventId = EventId(4178);

/// Audit event: a volume was force-unmounted, revoking the descriptors still
/// holding its nodes and deliberately discarding whatever uncommitted data
/// its journal retained (or had already lost). Carries the number of nodes
/// revoked, the discarded byte count, and why a plain detach was refused
/// (drives.md `fs.hotplug.force_unmount`).
const VOLUME_FORCE_UNMOUNTED: EventId = EventId(4179);

/// Audit event: a re-inserted volume was proven unmutated and its
/// retained uncommitted writes were replayed and committed; the volume is
/// back in service (drives.md `fs.hotplug.reinsert_replayed`). Carries the
/// replayed byte count.
const VOLUME_REINSERT_REPLAYED: EventId = EventId(4185);

/// Audit event: a re-inserted volume's non-mutation could not be proven
/// (or retention had been abandoned): it is mounted fresh and read-only
/// with the retained set kept for explicit salvage or force-discard
/// (drives.md `fs.hotplug.reinsert_conflict`). Carries the refusing cause
/// and the retained byte count still held.
const VOLUME_REINSERT_CONFLICT: EventId = EventId(4186);

/// Audit event: a runtime volume was attached and its node published; it
/// is mounted once its format's driver serves it (drives.md
/// `fs.serve.awaiting`).
const VOLUME_AWAITING_DRIVER: EventId = EventId(4192);

/// Audit event: a driver serves a volume again, after the last one was lost
/// or its medium came back (drives.md `fs.serve.resumed`).
const VOLUME_SERVE_RESUMED: EventId = EventId(4193);

/// Audit event: a volume could not be served — its driver could not open it,
/// or the kernel could not mount what the driver offered — and was retracted
/// (drives.md `fs.serve.refused`). `cause` says which, `error` why, and
/// `discarded_bytes` what of its device writes could not be committed; a
/// conflicted volume is kept instead, its retained set the force-unmount's.
const VOLUME_SERVE_REFUSED: EventId = EventId(4194);

/// Audit event: the driver serving a volume is gone; the volume stays
/// mounted behind the fail-closed stand-in until a driver serves it again
/// (drives.md `fs.serve.lost`). The `cause` field says whether writes it held
/// unpublished were lost with it.
const VOLUME_SERVER_LOST: EventId = EventId(4195);

/// Base of the runtime volume mount-handle space (`"VOL"` tagged). Fresh
/// handles are minted from here, disjoint from the boot volumes' fixed
/// handles by construction.
const VOLUME_HANDLE_BASE: u64 = 0x564F_4C00_0000_0000;

/// One attached runtime volume's availability.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Availability {
    /// The serving driver is live; the volume operates normally.
    Available,
    /// The serving driver vanished with uncommitted writes retained in
    /// the journal: the root stays visible, new I/O fails closed, and
    /// the retained set awaits verified re-insert or force-discard.
    UnavailableDirty,
    /// The serving driver vanished after retention was abandoned:
    /// uncommitted data existed that the journal does not hold.
    UnavailableLost,
    /// The volume was re-inserted but non-mutation could not be proven:
    /// it is mounted fresh and read-only over a live driver while the
    /// retained set stays held for the audited force-discard (D4c).
    RecoveryConflict,
}

impl Availability {
    /// Whether the device the volume was attached over is still there.
    const fn device_present(self) -> bool {
        matches!(self, Self::Available | Self::RecoveryConflict)
    }
}

/// One attached runtime volume: the facts detach and surprise removal
/// need.
struct AttachedVolume {
    /// The volume's published 16-byte identity.
    id: [u8; 16],
    /// The catalog name the root is projected under.
    name: String,
    /// The mount-table entry's path (`/Storage/<name>`).
    path: Path,
    /// The mount's driver handle in the registry.
    handle: DriverHandle,
    /// The block-service endpoint, for the detach-time device flush and
    /// the surprise-removal match.
    endpoint: u64,
    /// The shared data window's region id, for the detach-time device
    /// flush.
    window: u64,
    /// The volume's uncommitted-write journal, shared with the
    /// [`JournaledBlock`] its served volume writes through.
    journal: Arc<SpinLock<RetainedWrites>>,
    /// The format family the kernel's probe recognised.
    fstype: VolumeFsType,
    /// Its place in the mount table.
    mount: Mount,
    /// The object its volume grant names, until it leaves service: let go
    /// then, so the window goes with its last transfer.
    served: Option<Arc<ServedVolume>>,
    /// The volume node published for its driver, with the node it was
    /// published under; `None` once withdrawn.
    node: Option<(u32, u32)>,
    /// The attached extent's first device LBA — the frame of reference
    /// the journal's retained LBAs and evidence window live in. A
    /// re-insert may replay only onto the identical extent.
    first_lba: u64,
    /// The attached extent's block count.
    blocks: u64,
    /// Whether its device is still there, and how it came back if it left.
    availability: Availability,
    /// An operation is building, recovering or retracting the volume: no
    /// other acts on the entry until it ends, and a surprise removal of its
    /// device is that operation's to apply as it does ([`VolumeClaim`]).
    claimed: bool,
}

impl AttachedVolume {
    /// Whether a driver's grant on `volume` names this volume, and a driver
    /// may still serve it.
    fn granted_as(&self, volume: u64) -> bool {
        self.served
            .as_ref()
            .is_some_and(|served| served.id() == volume && !served.retired())
    }
}

/// A volume's place in the mount table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Mount {
    /// No driver has served it yet, so it is not mounted.
    Pending,
    /// Mounted as `format`, served over the endpoint `server` while a
    /// driver serves it.
    Mounted {
        format: FormatName,
        server: Option<u64>,
    },
}

impl Mount {
    /// The endpoint a driver serves the volume over, while one does.
    const fn server(self) -> Option<u64> {
        match self {
            Self::Mounted { server, .. } => server,
            Self::Pending => None,
        }
    }

    /// What the mount table shows the volume as, once it is mounted.
    const fn format(self) -> Option<FormatName> {
        match self {
            Self::Mounted { format, .. } => Some(format),
            Self::Pending => None,
        }
    }
}

/// The fail-closed stand-in for a volume no operation may reach: registered
/// under an unavailable volume's handle, and put in a detached volume's place
/// for an operation that resolved it before it left. Every operation reports
/// a device fault, so no cached bytes, listings, or metadata of the medium are
/// ever served as live (drives.md §10 — removal fails existing handles per
/// their object semantics). Replacing the real driver also drops its
/// plaintext cache with it.
///
/// Every method of every facet is written out, including the ones the ABI
/// defaults: a default would answer `Unsupported`, reporting a vanished
/// disk as a *permanent* format limit ("this volume has no symbolic
/// links") instead of the transient fault it is, and a caller cannot tell
/// those apart.
struct UnavailableFs;

impl FilesystemRead for UnavailableFs {
    fn root(&self) -> NodeId {
        // A real-looking root, so an operation is routed *into* the stub
        // and faults honestly (`NodeId::NONE` would read as "volume not
        // online yet" instead of "device gone").
        NodeId::from_raw(1)
    }

    fn name_matching(&self) -> NameMatching {
        NameMatching::Exact
    }

    fn node_identity(&self) -> NodeIdentity {
        NodeIdentity::Stable
    }

    fn node_info(&mut self, _node: NodeId) -> Result<NodeInfo, DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn lookup(&mut self, _dir: NodeId, _name: &[u8]) -> Result<NodeId, DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn parent(&mut self, _dir: NodeId) -> Result<NodeId, DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn read_at(
        &mut self,
        _file: NodeId,
        _offset: u64,
        _buf: &mut [u8],
    ) -> Result<usize, DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn read_dir(
        &mut self,
        _dir: NodeId,
        _cursor: u64,
        _after: &[u8],
        _visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn seek_region(
        &mut self,
        _file: NodeId,
        _offset: u64,
        _region: SeekRegion,
    ) -> Result<Option<u64>, DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn read_link(&mut self, _link: NodeId, _out: &mut [u8]) -> Result<usize, DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn pin(&mut self, _node: NodeId) -> Result<NodeId, DriverError> {
        Err(DriverError::DeviceFault)
    }

    /// A hold on the driver this stands in for went with it, so letting one
    /// go always succeeds.
    fn unpin(&mut self, _pinned: NodeId) -> Result<Unpinned, DriverError> {
        Ok(Unpinned::Released)
    }

    fn release_pins(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

impl FilesystemWrite for UnavailableFs {
    fn create(
        &mut self,
        _dir: NodeId,
        _name: &[u8],
        _kind: NodeKind,
        _record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn create_link(
        &mut self,
        _dir: NodeId,
        _name: &[u8],
        _target: &[u8],
        _record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn link(&mut self, _dir: NodeId, _name: &[u8], _node: NodeId) -> Result<(), DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn write_at(
        &mut self,
        _file: NodeId,
        _offset: u64,
        _data: &[u8],
    ) -> Result<usize, DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn truncate(&mut self, _file: NodeId, _size: u64) -> Result<(), DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn punch(&mut self, _file: NodeId, _offset: u64, _len: u64) -> Result<(), DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn remove(&mut self, _dir: NodeId, _name: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn rename(
        &mut self,
        _src_dir: NodeId,
        _src_name: &[u8],
        _dst_dir: NodeId,
        _dst_name: &[u8],
    ) -> Result<(), DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Err(DriverError::DeviceFault)
    }
}

impl FilesystemSecurity for UnavailableFs {
    fn security(&mut self, _node: NodeId) -> Result<NodeSecurity, DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn set_security(&mut self, _node: NodeId, _security: NodeSecurity) -> Result<(), DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn records_ownership(&mut self, _node: NodeId) -> Result<bool, DriverError> {
        Err(DriverError::DeviceFault)
    }
}

impl FilesystemStats for UnavailableFs {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        Err(DriverError::DeviceFault)
    }
}

impl FilesystemAttrs for UnavailableFs {
    fn get_attr(
        &mut self,
        _node: NodeId,
        _key: &[u8],
        _value_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn set_attr(&mut self, _node: NodeId, _key: &[u8], _value: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn list_attr(
        &mut self,
        _node: NodeId,
        _index: u64,
        _key_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        Err(DriverError::DeviceFault)
    }

    fn remove_attr(&mut self, _node: NodeId, _key: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::DeviceFault)
    }
}

/// The facet is handed out rather than answered `None`, for the same
/// reason [`UnavailableFs::root`] returns a real-looking id: an
/// `fs_attr_*` call is routed *into* the stub and faults honestly. `None`
/// would say "this volume's format stores no attributes", which is a
/// claim about the format rather than about the missing device.
impl FilesystemAttrsProvider for UnavailableFs {
    fn attrs_fs(&mut self) -> Option<&mut dyn FilesystemAttrsFs> {
        Some(self)
    }
}

/// The boot-installed wiring the service operates with.
struct Wiring {
    audit: &'static (dyn Sink + Sync),
    pressure: &'static MemoryPressure,
    watches: Option<&'static WatchRegistry>,
    /// The tree volume nodes are published into.
    tree: &'static HwTreeStore,
}

/// The production runtime volume attach/detach service. One static
/// instance ([`VOLUME_SERVICE`]) is handed to the boot handover; until
/// [`install`](Self::install) wires it, every operation fails closed.
pub struct RuntimeVolumeService {
    /// Set-once boot wiring; fail-closed `NotImplemented` before install.
    wiring: OnceCell<Wiring>,
    /// The attached-volume registry. A spin lock, so the surprise-removal
    /// path can walk it from the endpoint-teardown context without
    /// parking; guards are held only for lookups and brief mutations,
    /// never across device I/O.
    state: SpinLock<Vec<AttachedVolume>>,
    /// The served block devices operations reach, one entry per
    /// block-service endpoint and window ([`DeviceSlot`]). The guard is held
    /// only for lookups and brief mutations, never across device I/O, and
    /// is taken inside the registry guard, never around it.
    devices: SpinLock<Vec<DeviceSlot>>,
    /// The volumes drivers serve, in ascending order of the id their grants
    /// name. Taken alone or inside the registry guard, and held only for a
    /// lookup or an edit.
    served: SpinLock<Vec<Arc<ServedVolume>>>,
    /// The next mount handle to mint.
    next_handle: AtomicU64,
    /// The next served-volume id to mint.
    next_served: AtomicU64,
    /// The volumes network filesystem servers offer and serve.
    network: network::NetworkVolumes,
}

/// One served block device's place in the registry: the lock every whole
/// operation on it holds, and its client once the first one connected it.
struct DeviceSlot {
    endpoint: u64,
    window: u64,
    /// Held across each attach, recovery and detach on this device, which
    /// all park on its I/O; an operation on another device never waits on it.
    op: Arc<SleepLock<()>>,
    /// The one client every volume on the device shares ([`SharedDevice`]).
    connected: Option<SharedDevice>,
    /// Operations holding the slot ([`DevicePin`]); a pinned slot is never
    /// retired, so its client cannot close under an operation using it.
    pins: usize,
    /// The publication the slot's window was conferred through was
    /// retracted: its medium left, though the endpoint lives on. Never
    /// cleared, so a volume an operation enters over the slot later is
    /// retracted however the two raced.
    retracted: bool,
}

/// An operation's hold on a [`DeviceSlot`], released on drop.
struct DevicePin<'a> {
    service: &'a RuntimeVolumeService,
    endpoint: u64,
    window: u64,
    op: Arc<SleepLock<()>>,
}

impl Drop for DevicePin<'_> {
    fn drop(&mut self) {
        let mut devices = self.service.devices.lock();
        if let Some(slot) = devices
            .iter_mut()
            .find(|slot| slot.endpoint == self.endpoint && slot.window == self.window)
        {
            slot.pins = slot.pins.saturating_sub(1);
        }
        drop(devices);
        self.service.retire_unused_devices();
    }
}

/// An operation's claim on one registry entry ([`AttachedVolume::claimed`]),
/// released on drop. A surprise removal the claim deferred is applied then.
struct VolumeClaim<'a> {
    service: &'a RuntimeVolumeService,
    id: [u8; 16],
}

impl Drop for VolumeClaim<'_> {
    fn drop(&mut self) {
        let (present_on, server) = {
            let mut state = self.service.state.lock();
            state
                .iter_mut()
                .find(|v| v.id == self.id)
                .map_or((None, None), |v| {
                    v.claimed = false;
                    let present_on = v
                        .availability
                        .device_present()
                        .then_some((v.endpoint, v.window));
                    (present_on, v.mount.server())
                })
        };
        // Asked outside the registry guard: the endpoint registry calls the
        // vanish observer, which takes that guard, under its own.
        if let Some((endpoint, window)) = present_on {
            if !tairix_kernel_core::callreg::contains(EndpointId(endpoint)) {
                self.service.handle_vanished(endpoint, Removal::Endpoint);
            } else if self.service.retracted(endpoint, window) {
                self.service.handle_vanished(endpoint, Removal::Publication);
            }
        }
        if let Some(server) =
            server.filter(|&server| !tairix_kernel_core::callreg::contains(EndpointId(server)))
        {
            self.service.server_lost(server);
        }
    }
}

/// What a surprise removal took from the volumes attached over an endpoint.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Removal {
    /// The serving driver died, and every publication it served with it.
    Endpoint,
    /// The driver retracted a publication and lives on: only the volumes
    /// on a retracted [`DeviceSlot`] lost their medium.
    Publication,
}

/// How a detach's claim on its volume went.
enum DetachClaim<'a> {
    /// Claimed, with the entry's facts.
    Held(VolumeClaim<'a>, DetachTarget),
    /// The volume left the registry while the detach waited for its device.
    Gone,
    /// Another operation holds the volume, or moved it to a device this
    /// detach does not hold; the volume's name for the refusal.
    Busy(String),
}

/// The facts of a volume awaiting its driver a serve or a refusal acts on,
/// snapshotted out of the registry so the work below never runs under its
/// guard.
struct ServeTarget {
    id: [u8; 16],
    name: String,
    path: Path,
    handle: DriverHandle,
    endpoint: u64,
    window: u64,
    journal: Arc<SpinLock<RetainedWrites>>,
    fstype: VolumeFsType,
    /// What the mount table shows it as, if a driver served it before.
    mounted_as: Option<FormatName>,
    served: Arc<ServedVolume>,
    conflicted: bool,
}

/// Publish into `tree` the node a volume of `fstype` is served through: its
/// format's driver binds it, and inherits the grant on served volume `served`
/// alone.
fn publish_volume_node(
    tree: &HwTreeStore,
    publisher: u32,
    fstype: VolumeFsType,
    served: u64,
) -> Result<u32, Errno> {
    let mut node = HwNode::new(0, publisher, HwDeviceClass::Storage);
    node.push_match_key(HwMatchKey::compatible(fstype.served_compatible())?)?;
    node.push_resource(HwResource::volume(served))?;
    tree.publish_child(publisher, node)
}

/// The facts of an attached volume a detach acts on, snapshotted out of the
/// registry so its device I/O never runs under the registry guard.
struct DetachTarget {
    name: String,
    handle: DriverHandle,
    endpoint: u64,
    window: u64,
    journal: Arc<SpinLock<RetainedWrites>>,
    availability: Availability,
    /// A driver serves it, whose filesystem state the detach flushes first.
    served: bool,
}

/// The one production service instance the boot handover installs.
pub static VOLUME_SERVICE: RuntimeVolumeService = RuntimeVolumeService::new();

impl RuntimeVolumeService {
    /// An unwired service; every operation fails closed until
    /// [`install`](Self::install).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            wiring: OnceCell::new(),
            state: SpinLock::new(Vec::new()),
            devices: SpinLock::new(Vec::new()),
            served: SpinLock::new(Vec::new()),
            next_handle: AtomicU64::new(VOLUME_HANDLE_BASE + 1),
            next_served: AtomicU64::new(1),
            network: network::NetworkVolumes::new(),
        }
    }

    /// Wire the service with the boot path's audit sink, pressure gauge,
    /// directory-watch registry and hardware tree. First-wins and idempotent,
    /// like the other late-installed seams.
    pub fn install(
        &self,
        audit: &'static (dyn Sink + Sync),
        pressure: &'static MemoryPressure,
        watches: Option<&'static WatchRegistry>,
        tree: &'static HwTreeStore,
    ) {
        let _ = self.wiring.set(Wiring {
            audit,
            pressure,
            watches,
            tree,
        });
    }

    /// The installed wiring, or fail-closed before boot installs it.
    fn wiring(&self) -> Result<&Wiring, Errno> {
        match self.wiring.get() {
            Ok(Some(wiring)) => Ok(wiring),
            _ => Err(Errno::NotImplemented),
        }
    }

    /// Pin the slot of the device served over `endpoint` and `window`,
    /// registering it on first use.
    ///
    /// Slots are keyed by the endpoint **and** the window: sharing a client
    /// is only safe for consumers staging bytes in the same buffer, and a
    /// device presenting a different window is a different staging buffer,
    /// so it gets its own slot rather than being folded onto a stale one.
    /// Window ids are never reused, so a torn-down endpoint's client can
    /// never be handed to a device that later re-uses its id.
    fn pin_device(&self, endpoint: u64, window: u64) -> Result<DevicePin<'_>, Errno> {
        let mut devices = self.devices.lock();
        let found = devices
            .iter()
            .position(|slot| slot.endpoint == endpoint && slot.window == window);
        let at = if let Some(at) = found {
            at
        } else {
            devices.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
            let op = Arc::try_new(SleepLock::new(())).map_err(|_| Errno::OutOfMemory)?;
            devices.push(DeviceSlot {
                endpoint,
                window,
                op,
                connected: None,
                pins: 0,
                retracted: false,
            });
            devices.len() - 1
        };
        let slot = &mut devices[at];
        slot.pins += 1;
        Ok(DevicePin {
            service: self,
            endpoint,
            window,
            op: Arc::clone(&slot.op),
        })
    }

    /// The pinned device's shared client, connecting it on first use.
    ///
    /// Every volume on one disk shares the single connected client, because
    /// they share the one staging window the transfers move bytes through
    /// (see [`SharedDevice`]). The caller holds the device's operation lock,
    /// so no second client is ever connected over the window; the registry
    /// guard is never held across the connect.
    ///
    /// # Errors
    ///
    /// `(cause, errno)` pairs the attach path audits through its refusal
    /// helper.
    fn connected(
        &self,
        pin: &DevicePin<'_>,
        request: &VolumeAttachRequest<'_>,
        wiring: &Wiring,
    ) -> Result<SharedDevice, (&'static str, Errno)> {
        if let Some(device) = self.device_for(pin.endpoint, pin.window) {
            return Ok(device);
        }
        let device = connect_device(request, wiring)?;
        if let Some(slot) = self
            .devices
            .lock()
            .iter_mut()
            .find(|slot| slot.endpoint == pin.endpoint && slot.window == pin.window)
        {
            slot.connected = Some(device.clone());
        }
        Ok(device)
    }

    /// Whether the publication `window` was conferred through on `endpoint`
    /// was retracted.
    fn retracted(&self, endpoint: u64, window: u64) -> bool {
        slot_retracted(&self.devices.lock(), endpoint, window)
    }

    /// The connected shared device serving `endpoint` over `window`, if any.
    fn device_for(&self, endpoint: u64, window: u64) -> Option<SharedDevice> {
        let devices = self.devices.lock();
        devices
            .iter()
            .find(|slot| slot.endpoint == endpoint && slot.window == window)
            .and_then(|slot| slot.connected.clone())
    }

    /// Drop every device slot no operation holds and no volume on a present
    /// device names, closing its client and releasing its window hold. A
    /// volume held unavailable after its device vanished keeps its journal,
    /// never the device.
    ///
    /// The surprise-removal path runs in endpoint-teardown context and only
    /// marks volumes, so it never drops a client there; the next operation,
    /// or the last pin on the slot going, does.
    fn retire_unused_devices(&self) {
        let mut live: Vec<(u64, u64)> = {
            let state = self.state.lock();
            state
                .iter()
                .filter(|v| v.availability.device_present())
                .map(|v| (v.endpoint, v.window))
                .collect()
        };
        live.sort_unstable();
        // The retired slots are dropped *outside* the registry guard:
        // releasing the last reference closes the client and its window
        // hold, which must not run under a spin lock.
        let retired: Vec<DeviceSlot> = self
            .devices
            .lock()
            .extract_if(.., |slot| {
                slot.pins == 0 && live.binary_search(&(slot.endpoint, slot.window)).is_err()
            })
            .collect();
        drop(retired);
    }

    /// Enter `volume` into the registry claimed by the attach building it.
    ///
    /// # Errors
    ///
    /// `(cause, errno)` pairs the attach path audits: a volume of the same
    /// identity is registered already (`Busy`, as a published one is), or
    /// the registry cannot grow.
    fn enter(&self, volume: AttachedVolume) -> Result<VolumeClaim<'_>, (&'static str, Errno)> {
        let id = volume.id;
        let mut state = self.state.lock();
        if state.iter().any(|v| v.id == id) {
            return Err(("identity_attached", Errno::Busy));
        }
        state
            .try_reserve(1)
            .map_err(|_| ("registry_full", Errno::OutOfMemory))?;
        state.push(AttachedVolume {
            claimed: true,
            ..volume
        });
        Ok(VolumeClaim { service: self, id })
    }

    /// Whether an attached volume holds the catalog name `name`, mounted or
    /// still awaiting its driver.
    fn name_reserved(&self, name: &str) -> bool {
        self.state.lock().iter().any(|v| v.name == name)
    }

    /// Build the served volume over `window` and enter it under a fresh id.
    fn new_served(
        &self,
        window: VolumeWindow,
        read_only: bool,
    ) -> Result<Arc<ServedVolume>, Errno> {
        let id = self.next_served.fetch_add(1, Ordering::Relaxed);
        let served = Arc::try_new(ServedVolume::new(id, window, read_only)?)
            .map_err(|_| Errno::OutOfMemory)?;
        let mut registry = self.served.lock();
        registry.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        let at = registry.partition_point(|v| v.id() < id);
        registry.insert(at, Arc::clone(&served));
        Ok(served)
    }

    /// The served volume a grant names.
    fn served(&self, volume: u64) -> Result<Arc<ServedVolume>, Errno> {
        let registry = self.served.lock();
        registry
            .binary_search_by_key(&volume, |v| v.id())
            .map(|at| Arc::clone(&registry[at]))
            .map_err(|_| Errno::NotFound)
    }

    /// The device the volume a grant on `volume` names lives on.
    fn served_device(&self, volume: u64) -> Result<(u64, u64), Errno> {
        self.state
            .lock()
            .iter()
            .find(|v| v.granted_as(volume))
            .map(|v| (v.endpoint, v.window))
            .ok_or(Errno::NotFound)
    }

    /// Take `served` out of service and out of the registry, and withdraw
    /// `node`, so its driver is unloaded. The grant the driver holds on it
    /// names nothing from here on, and a served volume's id is never reused.
    fn retire_served(&self, served: Option<&ServedVolume>, node: Option<(u32, u32)>) {
        if let Some(served) = served {
            served.retire();
            let mut registry = self.served.lock();
            if let Ok(at) = registry.binary_search_by_key(&served.id(), |v| v.id()) {
                registry.remove(at);
            }
        }
        if let (Some((publisher, node)), Ok(wiring)) = (node, self.wiring()) {
            // Gone already when the device's own node left, taking it along.
            let _ = wiring.tree.remove_child(publisher, node);
        }
    }

    /// Record the node published for volume `id`.
    fn set_node(&self, id: &[u8; 16], node: Option<(u32, u32)>) {
        if let Some(volume) = self.state.lock().iter_mut().find(|v| v.id == *id) {
            volume.node = node;
        }
    }

    /// Retract volume `id` from the registry and from everywhere it reached.
    fn retract(&self, id: &[u8; 16]) {
        let entry = {
            let mut state = self.state.lock();
            state
                .iter()
                .position(|v| v.id == *id)
                .map(|at| state.remove(at))
        };
        if let Some(entry) = entry {
            self.withdraw(&entry);
        }
    }

    /// Withdraw what `entry`, out of the registry, reached: its served volume
    /// and its node, and once it was mounted its mount, its registered driver
    /// and its published identity.
    fn withdraw(&self, entry: &AttachedVolume) {
        self.retire_served(entry.served.as_deref(), entry.node);
        if entry.mount.format().is_some() {
            if let Ok(vfs) = LATE_FILESYSTEM.vfs() {
                let _ = vfs
                    .mounts_write()
                    .detach(entry.handle, BindsWithin::Withdraw);
            }
            let _ = LATE_FILESYSTEM.unregister(entry.handle);
            let _ = VOLUME_FOREST.unpublish(&entry.id);
        }
    }

    /// Claim the volume awaiting a driver that a grant on `volume` names, on
    /// `device`.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no such volume there; [`Errno::AlreadyExists`]
    /// for one a driver serves already; [`Errno::Busy`] for one another
    /// operation holds.
    fn claim_awaiting(
        &self,
        volume: u64,
        device: (u64, u64),
    ) -> Result<(VolumeClaim<'_>, ServeTarget), Errno> {
        let mut state = self.state.lock();
        let entry = state
            .iter_mut()
            .find(|v| v.granted_as(volume) && (v.endpoint, v.window) == device)
            .ok_or(Errno::NotFound)?;
        let served = entry.served.clone().ok_or(Errno::NotFound)?;
        if entry.mount.server().is_some() {
            return Err(Errno::AlreadyExists);
        }
        if entry.claimed {
            return Err(Errno::Busy);
        }
        let name = concatenated(&[&entry.name]).ok_or(Errno::OutOfMemory)?;
        let path = entry.path.owned().map_err(VfsError::to_errno)?;
        entry.claimed = true;
        let target = ServeTarget {
            id: entry.id,
            name,
            path,
            handle: entry.handle,
            endpoint: entry.endpoint,
            window: entry.window,
            journal: Arc::clone(&entry.journal),
            fstype: entry.fstype,
            mounted_as: entry.mount.format(),
            served,
            conflicted: entry.availability == Availability::RecoveryConflict,
        };
        Ok((
            VolumeClaim {
                service: self,
                id: entry.id,
            },
            target,
        ))
    }

    /// Claim the volume `id` for a detach holding the device `held` (`None`
    /// for one whose device is gone): it must still be on that device, or
    /// have lost its device, and no other operation may hold it.
    fn claim_for_detach(&self, id: &[u8; 16], held: Option<(u64, u64)>) -> DetachClaim<'_> {
        let mut state = self.state.lock();
        let Some(volume) = state.iter_mut().find(|v| v.id == *id) else {
            return DetachClaim::Gone;
        };
        let on_held =
            !volume.availability.device_present() || held == Some((volume.endpoint, volume.window));
        if volume.claimed || !on_held {
            return DetachClaim::Busy(volume.name.clone());
        }
        volume.claimed = true;
        let target = DetachTarget {
            name: volume.name.clone(),
            handle: volume.handle,
            endpoint: volume.endpoint,
            window: volume.window,
            journal: Arc::clone(&volume.journal),
            availability: volume.availability,
            served: volume.mount.server().is_some(),
        };
        DetachClaim::Held(
            VolumeClaim {
                service: self,
                id: *id,
            },
            target,
        )
    }

    /// The name of volume `id` and the device it is attached over: `None`
    /// when no such volume is attached, and no device when its device is
    /// gone.
    fn attached_device(&self, id: &[u8; 16]) -> Option<(String, Option<(u64, u64)>)> {
        let state = self.state.lock();
        let volume = state.iter().find(|v| v.id == *id)?;
        Some((
            volume.name.clone(),
            volume
                .availability
                .device_present()
                .then_some((volume.endpoint, volume.window)),
        ))
    }

    /// Mount what `offer` describes, served over `channel`, for `target`: in
    /// the mount table for the first time, or in place of the stand-in of a
    /// volume a driver served before.
    ///
    /// # Errors
    ///
    /// `(cause, errno)` pairs the serve path audits as it retracts the
    /// volume.
    fn mount_served(
        target: &ServeTarget,
        offer: &ServeOffer,
        channel: ServeChannel,
        device: &SharedDevice,
        wiring: &Wiring,
    ) -> Result<(), (&'static str, Errno)> {
        // The kernel named the format from its own probe; a driver names
        // only the generation it is.
        if offer.format.family() != Some(target.fstype) {
            return Err(("format_mismatch", Errno::BadMagic));
        }
        let vfs = LATE_FILESYSTEM
            .vfs()
            .map_err(|err| ("no_mount_table", err))?;
        let fs = ServedFs::new(
            &channel.endpoint,
            channel.window,
            *offer,
            &target.served.activity(),
            wiring.audit,
        )
        .map_err(|err| ("proxy_unbuilt", err))?;
        let state = fs.state();
        let map_gid = identity_map_gid(target.fstype);
        let handle = target.handle.as_u64();
        let (driver, watch) = match map_gid {
            Some(gid) => cached(
                PresentedFs::new(fs, Presented::removable(gid)),
                handle,
                target.id,
                wiring.pressure,
                wiring.audit,
                wiring.watches,
            ),
            None => cached(
                fs,
                handle,
                target.id,
                wiring.pressure,
                wiring.audit,
                wiring.watches,
            ),
        }
        .map_err(|err| ("cache_unbuilt", err))?;
        let read_only = target.served.read_only() || offer.write_refusal.is_some();
        let attachment = Attachment::new(target.handle, device.medium, mount_flags(read_only));
        let template = mount_template(map_gid);
        let path = held_path(&target.path)?;
        let register = |driver, watch| {
            register_with_health(
                target.handle,
                driver,
                watch,
                &target.name,
                offer.format.as_str(),
                target.id,
                device.health.clone(),
            )
            .map_err(|()| ("handle_in_use", Errno::Busy))
        };
        if target.mounted_as.is_some() {
            // The posture goes first, under the stand-in's lock while it still
            // answers the mount: no operation reaches the live driver under the
            // posture it replaces, and one that takes the lock after the move
            // finds its projection gone. The volume returns under a fresh
            // registration, so a descriptor held across the loss answers stale.
            let standing = LATE_FILESYSTEM.driver(target.handle).ok();
            let _held = standing.as_deref().map(DriverLock::lock);
            reattach(vfs, attachment, path, template)?;
            let _ = LATE_FILESYSTEM.unregister(target.handle);
            register(driver, watch)?;
        } else {
            vfs.mounts_write()
                .attach_at(attachment, path, template)
                .map_err(|_| ("name_in_use", Errno::AlreadyExists))?;
            let unmount = || {
                let _ = vfs
                    .mounts_write()
                    .detach(target.handle, BindsWithin::Withdraw);
            };
            // Published before the driver is reachable, so a refusal has no
            // operation to turn away: one would wait on a driver that is
            // inside this very call.
            publish_volume_identity(
                target.id,
                &["Storage", &target.name],
                &target.name,
                wiring.audit,
            )
            .map_err(|err| {
                unmount();
                (
                    "identity_unpublishable",
                    match err {
                        VolumePublishError::NilIdentity => Errno::OutOfRange,
                        VolumePublishError::AlreadyPublished => Errno::Busy,
                    },
                )
            })?;
            register(driver, watch).inspect_err(|_| {
                unmount();
                let _ = VOLUME_FOREST.unpublish(&target.id);
            })?;
        }
        let _ = LATE_FILESYSTEM.set_served_state(target.handle, Some(state));
        let _ = LATE_FILESYSTEM.set_availability(
            target.handle,
            if target.conflicted {
                MountAvailability::RecoveryConflict
            } else {
                MountAvailability::Available
            },
        );
        Ok(())
    }

    /// Retract `target`, which no driver will serve, for `cause` and `error`,
    /// committing first what its device holds of it.
    ///
    /// A conflicted volume is kept: its retained set is the audited
    /// force-unmount's to discard, never a refusal's, so only its served
    /// volume and node go and the stand-in keeps answering its mount.
    fn retract_unserved(
        &self,
        target: &ServeTarget,
        audit: &'static (dyn Sink + Sync),
        cause: &'static str,
        error: Errno,
    ) {
        let discarded = if target.conflicted {
            let node = self
                .state
                .lock()
                .iter_mut()
                .find(|v| v.id == target.id)
                .and_then(|v| {
                    v.served = None;
                    v.node.take()
                });
            self.retire_served(Some(&target.served), node);
            None
        } else {
            let flushed = self
                .device_for(target.endpoint, target.window)
                .map(|device| device.window().flush());
            let bytes = {
                let mut journal = target.journal.lock();
                if matches!(flushed, Some(Ok(()) | Err(DriverError::Unsupported))) {
                    journal.commit();
                    0
                } else {
                    let bytes = journal.retained_bytes();
                    journal.discard_all();
                    bytes
                }
            };
            self.retract(&target.id);
            Some(bytes)
        };
        let volume = Field {
            key: "volume",
            value: FieldValue::Str(&target.name),
        };
        let cause = Field {
            key: "cause",
            value: FieldValue::Str(cause),
        };
        let error = Field {
            key: "error",
            value: FieldValue::Error(error),
        };
        match discarded {
            Some(bytes) => {
                let discarded = Field {
                    key: "discarded_bytes",
                    value: FieldValue::UnsignedInt(bytes),
                };
                log(
                    audit,
                    &Event {
                        level: Level::Warn,
                        id: VOLUME_SERVE_REFUSED,
                        message: "volume-service: the volume could not be served; retracted",
                        fields: &[volume, cause, error, discarded],
                    },
                );
            }
            None => {
                log(
                    audit,
                    &Event {
                        level: Level::Warn,
                        id: VOLUME_SERVE_REFUSED,
                        message: "volume-service: the conflicted volume could not be served; \
                                  kept read-only for its force-unmount",
                        fields: &[volume, cause, error],
                    },
                );
            }
        }
    }

    /// Audit a refused attach and hand back its errno.
    fn refuse_attach(
        audit: &'static (dyn Sink + Sync),
        name: &str,
        cause: &'static str,
        errno: Errno,
    ) -> Errno {
        Self::audit_event(
            audit,
            VOLUME_ATTACH_REFUSED,
            "volume-service: runtime volume attach refused",
            name,
            Some(cause),
        );
        errno
    }

    /// Audit a refused detach and hand back its errno.
    fn refuse_detach(
        audit: &'static (dyn Sink + Sync),
        name: &str,
        cause: &'static str,
        errno: Errno,
    ) -> Errno {
        Self::audit_event(
            audit,
            VOLUME_DETACH_REFUSED,
            DETACH_REFUSED,
            name,
            Some(cause),
        );
        errno
    }

    /// Audit a plain detach refused because descriptors hold `open` of the
    /// volume's nodes, and hand back its errno.
    fn refuse_open_nodes(audit: &'static (dyn Sink + Sync), name: &str, open: usize) -> Errno {
        log(
            audit,
            &Event {
                level: Level::Warn,
                id: VOLUME_DETACH_REFUSED,
                message: DETACH_REFUSED,
                fields: &[
                    Field {
                        key: "volume",
                        value: FieldValue::Str(name),
                    },
                    Field {
                        key: "cause",
                        value: FieldValue::Str("nodes_open"),
                    },
                    Field {
                        key: "open_nodes",
                        value: FieldValue::UnsignedInt(open as u64),
                    },
                ],
            },
        );
        Errno::Busy
    }

    /// Audit a catalog name another volume holds. Not a refusal of the
    /// volume: the volume manager tries its next candidate on this answer.
    fn name_taken(audit: &'static (dyn Sink + Sync), name: &str) -> Errno {
        Self::audit_event(
            audit,
            VOLUME_NAME_TAKEN,
            "volume-service: catalog name taken; the volume manager tries its next",
            name,
            None,
        );
        Errno::AlreadyExists
    }

    /// Audit why a volume asked for writable was mounted read-only, if its
    /// driver refused to write it.
    fn audit_write_refusal(
        audit: &'static (dyn Sink + Sync),
        name: &str,
        write_refusal: Option<WriteRefusal>,
    ) {
        if let Some(cause) = write_refusal {
            Self::audit_event(
                audit,
                VOLUME_ATTACHED_READ_ONLY,
                "volume-service: the filesystem refuses to write this volume; mounted read-only",
                name,
                Some(cause.as_str()),
            );
        }
    }

    /// Audit one attach/detach decision with the volume's catalog name (or
    /// its identity's short hex for a detach that never resolved a name).
    fn audit_event(
        audit: &'static (dyn Sink + Sync),
        id: EventId,
        message: &'static str,
        name: &str,
        cause: Option<&'static str>,
    ) {
        let name_field = Field {
            key: "volume",
            value: FieldValue::Str(name),
        };
        match cause {
            Some(cause) => {
                log(
                    audit,
                    &Event {
                        level: Level::Warn,
                        id,
                        message,
                        fields: &[
                            name_field,
                            Field {
                                key: "cause",
                                value: FieldValue::Str(cause),
                            },
                        ],
                    },
                );
            }
            None => {
                log(
                    audit,
                    &Event {
                        level: Level::Info,
                        id,
                        message,
                        fields: &[name_field],
                    },
                );
            }
        }
    }
}

impl Default for RuntimeVolumeService {
    fn default() -> Self {
        Self::new()
    }
}

/// The disk stack every runtime volume is served over: its extent of the one
/// shared, cached device, behind the uncommitted-write journal.
pub(crate) type VolumeWindow =
    PartitionBlock<JournaledBlock<OwnedBlockWindow<BlockCache<BlkClient>>>>;

/// A copy of `path` the heap can hold.
fn held_path(path: &Path) -> Result<Path, (&'static str, Errno)> {
    path.owned()
        .map_err(|_| ("path_unallocated", Errno::OutOfMemory))
}

/// Where the catalog shows a volume named `name`.
fn storage_path(name: &str) -> Result<Path, (&'static str, Errno)> {
    Path::parse(&format!("/Storage/{name}")).map_err(|_| ("mount_path_invalid", Errno::OutOfRange))
}

/// Move the mount at `path` to `attachment`'s posture. The binds standing
/// within the volume go with the projection they stood in.
///
/// A refusal leaves the path unmounted — fail closed — and cannot occur
/// while the volume's claim is held: only that operation touches the handle,
/// and the path is unmounted and mounted again under one mount-table guard.
fn reattach(
    vfs: &Vfs,
    attachment: Attachment,
    path: Path,
    template: Metadata,
) -> Result<(), (&'static str, Errno)> {
    let mut mounts = vfs.mounts_write();
    let _ = mounts.detach(attachment.driver(), BindsWithin::Withdraw);
    mounts
        .attach_at(attachment, path, template)
        .map_err(|_| ("mount_path_invalid", Errno::Busy))
}

/// Register `driver`, a user-space server's proxy, under `handle`, its
/// releases reaped by the operations on the volume, and attach the live
/// block-health overlay the mount snapshot reads (`plans/FIX-IO.md`
/// IO2/IO3): the one place a served volume is registered, so the wiring
/// cannot diverge between a first serve and a later one. Returns `Err(())`
/// when the handle is already registered (fail closed — the caller unwinds);
/// the `set_io_source` cannot miss, the handle having just been registered by
/// the one operation that holds it.
fn register_with_health(
    handle: DriverHandle,
    driver: Box<dyn KernelFs>,
    watch: Option<ClaimRef>,
    source: &str,
    fstype: &str,
    volume_id: [u8; 16],
    health: VolumeIoSource,
) -> Result<(), ()> {
    LATE_FILESYSTEM
        .register_served(handle, driver, source, fstype, volume_id, watch)
        .map_err(|_| ())?;
    let _ = LATE_FILESYSTEM.set_io_source(handle, health);
    Ok(())
}

/// One served block device, connected once and shared by every runtime
/// volume that lives on it.
///
/// A disk carries several volumes (its partitions), and each of them drives
/// the **same** served device over the **same** single shared data window:
/// the block-service protocol stages each transfer's bytes in that window, so
/// two clients issuing requests over it concurrently would overwrite each
/// other's staged bytes and hand a reader another volume's data. The device
/// is therefore connected once per block-service endpoint and reached through
/// [`SharedBlock`], whose sleeping lock serialises whole device operations —
/// so the window holds exactly one transfer at a time by construction.
///
/// Sharing the client also gives a disk one health fold rather than a
/// divergent copy per volume: the reported-health overlay and the I/O
/// counters are properties of the device, not of a mount.
#[derive(Clone)]
struct SharedDevice {
    /// The block-service endpoint the device is served over, whose
    /// registration tells a vanished device from a failing one.
    endpoint: u64,
    /// The one client, shared and serialised, behind the disk cache.
    device: Arc<SharedBlock<BlockCache<BlkClient>>>,
    /// The device's write policy, read once at connect.
    read_only: bool,
    /// The storage medium the device declared at connect, carried onto every
    /// mount that lives on it so a volume reports the drive it is really on.
    /// `None` is a device that declared no medium this build recognises,
    /// which the mount reports as unknown rather than as a guessed one.
    medium: Option<BlkMedium>,
    /// The device's reported-health overlay, folded once for every volume
    /// on the device.
    health: VolumeIoSource,
}

impl SharedDevice {
    /// A fresh window onto the shared device for one mount to own.
    fn window(&self) -> OwnedBlockWindow<BlockCache<BlkClient>> {
        self.device.owned_handle()
    }

    /// The device's block size, from the geometry cached at connect.
    fn block_size(&self) -> u32 {
        self.device.geometry().block_size
    }
}

/// Connect the kernel blkio client for `request` and validate the
/// requested extent against the live geometry.
///
/// # Errors
///
/// `(cause, errno)` pairs the attach path audits through its refusal
/// helper.
fn connect_device(
    request: &VolumeAttachRequest<'_>,
    wiring: &Wiring,
) -> Result<SharedDevice, (&'static str, Errno)> {
    // Reach the shared data window through the kernel's counted hold.
    let hold = kernel_hold(installed_shared_mem_facility(), request.window)
        .map_err(|err| ("window_unreachable", err))?;
    // Connect the kernel blkio client and validate the device geometry.
    let client = BlkClient::connect(request.endpoint, hold, wiring.audit)
        .map_err(|err| ("endpoint_unusable", err))?;
    let read_only = client.read_only();
    let medium = client.medium();
    let health = client.io_source();
    // Every volume on the disk reads through one coherent cache of its
    // recently used blocks, on the device side of the sharing lock, so a
    // filesystem's metadata reads stop costing a device round trip each.
    let client = BlockCache::for_disk(client, REMOVABLE_DISK_OWNER, wiring.pressure, wiring.audit)
        .map_err(|err| ("geometry_unreadable", err.as_errno()))?;
    if let Some(ledger) = client.ledger() {
        tairix_kernel_core::memstats::MEM_STATS.register_ledger(ledger);
    }
    // Wrapping caches the geometry, so the shared device answers it
    // lock-free; a device whose geometry cannot be read is never wrapped.
    let device = SharedBlock::new(client).map_err(|err| ("geometry_unreadable", err.as_errno()))?;
    Ok(SharedDevice {
        endpoint: request.endpoint,
        device: Arc::try_new(device).map_err(|_| ("device_unallocated", Errno::OutOfMemory))?,
        read_only,
        medium,
        health,
    })
}

/// Bound the requested extent by the device's live geometry.
///
/// # Errors
///
/// The `(cause, errno)` pair the attach path audits when the extent runs
/// past the end of the device (fail closed — never a clamped window).
fn validate_extent(
    request: &VolumeAttachRequest<'_>,
    device: &SharedDevice,
) -> Result<(), (&'static str, Errno)> {
    let block_count = device.device.geometry().block_count;
    if request
        .first_lba
        .checked_add(request.blocks)
        .is_none_or(|end| end > block_count)
    {
        return Err(("extent_out_of_range", Errno::LengthOutOfRange));
    }
    Ok(())
}

/// Read `len` bytes of whole blocks from device-absolute `lba`. `None` when
/// the allocation is refused or the device refuses the read — the caller
/// fails closed, never guesses.
fn read_blocks_at<B: Block>(client: &mut B, lba: u64, len: usize) -> Option<Vec<u8>> {
    let mut buf = tairix_util::fallible::filled(len, 0u8)?;
    client.read_blocks(lba, &mut buf).ok()?;
    Some(buf)
}

/// The mount-policy flags every runtime volume carries: `noexec`, so a
/// program carried in on removable media is not run, plus `ro` when the
/// device's write policy or the recovery posture demands it.
fn mount_flags(read_only: bool) -> MountFlags {
    if read_only {
        MountFlags::NOEXEC.union(MountFlags::READ_ONLY)
    } else {
        MountFlags::NOEXEC
    }
}

/// The mount point's permission template (a runtime mount point has no
/// node in the boot layout tree, so the template travels with the
/// mount). An identity-mapped volume's template matches the map —
/// system-owned under the storage group, group-writable — so the mount
/// point itself is as reachable as its content; every other volume gets
/// the system-owned world-traversable default, writes gated per inode by
/// the volume's own records.
fn mount_template(map_gid: Option<GroupId>) -> Metadata {
    match map_gid {
        Some(gid) => Presented::removable(gid).template(),
        None => Metadata::new(UserId(0), GroupId(0), Mode::from_bits(0o755)),
    }
}

/// The storage-group identity map an **ownerless** format is mounted
/// under (`plans/DEVICES.md` D3d); formats with a real owner model keep
/// their on-disk records.
fn identity_map_gid(fstype: VolumeFsType) -> Option<GroupId> {
    match fstype {
        VolumeFsType::Fat32 | VolumeFsType::Adfs | VolumeFsType::Iso9660 | VolumeFsType::Udf => {
            LATE_STORAGE_GID.get()
        }
        VolumeFsType::ARXFS | VolumeFsType::Ext4 => None,
    }
}

/// A fresh uncommitted-write journal for the extent, its
/// mutation-evidence shadow seeded from the window the recognised volume
/// declares (D4c). A volume with no declared window (or an unreadable one)
/// simply holds no evidence; a later re-insert then fails closed to the
/// conflict path. `None` when the journal cannot be allocated.
fn seeded_journal<B: Block>(
    client: &mut B,
    request: &VolumeAttachRequest<'_>,
    block_size: u32,
    evidence: Option<Evidence>,
) -> Option<Arc<SpinLock<RetainedWrites>>> {
    let journal = Arc::try_new(SpinLock::new(RetainedWrites::new(
        block_size,
        // Budget from discovered physical RAM (the growable kernel heap's
        // bootstrap size is no longer the memory to size a cache against);
        // falls back to the bootstrap size before RAM is published.
        CacheBudget::from_backing(tairix_kernel_core::memstats::cache_backing_bytes()),
    )))
    .ok()?;
    if let Some((lba, len)) =
        evidence.and_then(|evidence| evidence_blocks(request, block_size, evidence))
    {
        if let Some(bytes) = read_blocks_at(client, lba, len) {
            journal.lock().set_evidence(lba, &bytes);
        }
    }
    Some(journal)
}

/// The device blocks covering `evidence` in the requested extent, as the
/// first device-absolute LBA and the byte length: rounded out to whole
/// blocks, since the shadow compares blocks. `None` for a window the extent
/// cannot hold.
pub(crate) fn evidence_blocks(
    request: &VolumeAttachRequest<'_>,
    block_size: u32,
    evidence: Evidence,
) -> Option<(u64, usize)> {
    let bs = u64::from(block_size);
    let first = evidence.offset.checked_div(bs)?;
    let end = evidence.offset.checked_add(evidence.len)?.div_ceil(bs);
    if end <= first || end > request.blocks {
        return None;
    }
    let len = usize::try_from((end - first).checked_mul(bs)?).ok()?;
    Some((request.first_lba.checked_add(first)?, len))
}

/// What the requested extent holds, recognised exactly as the volume manager
/// recognised it. `None` for nothing attachable, a RAID member, or an extent
/// that cannot be read — the caller then holds no evidence and claims no
/// re-insert, failing closed.
fn recognise_extent<B: Block>(
    client: &mut B,
    request: &VolumeAttachRequest<'_>,
) -> Option<ProbedVolume> {
    let window = PartitionBlock::new(client, request.first_lba, request.blocks).ok()?;
    let mut extent = ByteDevice::new(window).ok()?;
    match tairix_fsprobe::recognise(&mut extent).ok()?? {
        Recognised::Volume(volume) => Some(volume),
        Recognised::RaidMember(_) => None,
    }
}

/// Audit how a re-insert resolved: the replayed recovery (with its byte
/// count) or the read-only conflict (with its cause and the retained
/// byte count still held).
fn audit_recovery_outcome(
    audit: &'static (dyn Sink + Sync),
    name: &str,
    outcome: &RecoveryOutcome,
    journal: &Arc<SpinLock<RetainedWrites>>,
) {
    match outcome {
        RecoveryOutcome::Replayed(bytes) => {
            log(
                audit,
                &Event {
                    level: Level::Info,
                    id: VOLUME_REINSERT_REPLAYED,
                    message: "volume-service: re-inserted volume proven unmutated; \
                              retained writes replayed and committed",
                    fields: &[
                        Field {
                            key: "volume",
                            value: FieldValue::Str(name),
                        },
                        Field {
                            key: "replayed_bytes",
                            value: FieldValue::UnsignedInt(*bytes),
                        },
                    ],
                },
            );
        }
        RecoveryOutcome::Conflict(cause) => {
            let retained = journal.lock().retained_bytes();
            log(
                audit,
                &Event {
                    level: Level::Warn,
                    id: VOLUME_REINSERT_CONFLICT,
                    message: "volume-service: re-inserted volume could not be proven \
                              unmutated; mounted read-only with the retained set kept",
                    fields: &[
                        Field {
                            key: "volume",
                            value: FieldValue::Str(name),
                        },
                        Field {
                            key: "cause",
                            value: FieldValue::Str(cause),
                        },
                        Field {
                            key: "retained_bytes",
                            value: FieldValue::UnsignedInt(retained),
                        },
                    ],
                },
            );
        }
    }
}

impl VolumeService for RuntimeVolumeService {
    fn attach(
        &self,
        request: &VolumeAttachRequest<'_>,
        publisher: Option<u32>,
    ) -> Result<(), Errno> {
        let wiring = self.wiring()?;
        let audit = wiring.audit;
        // The frame validated the name as short ASCII; re-derive the &str.
        let name = core::str::from_utf8(request.name).map_err(|_| Errno::OutOfRange)?;
        let refused = |cause, errno| Self::refuse_attach(audit, name, cause, errno);

        // The live mount table must exist (the boot volumes are up).
        let vfs = LATE_FILESYSTEM
            .vfs()
            .map_err(|err| refused("no_mount_table", err))?;
        self.retire_unused_devices();
        // Every volume on this disk drives the one shared client over the
        // one staging window, one operation at a time (see `DeviceSlot`).
        let pin = self
            .pin_device(request.endpoint, request.window)
            .map_err(|errno| refused("registry_full", errno))?;
        let _op = pin.op.lock();
        // A window outlives its publication only while an operation holds
        // it, and its client is answered nothing but stale.
        if self.retracted(pin.endpoint, pin.window) {
            return Err(refused("publication_retracted", Errno::Stale));
        }
        let device = self
            .connected(&pin, request, wiring)
            .map_err(|(cause, errno)| refused(cause, errno))?;
        validate_extent(request, &device).map_err(|(cause, errno)| refused(cause, errno))?;
        let block_size = device.block_size();
        let mut disk = device.window();

        // The kernel names the volume from its own probe, never from the
        // driver that will serve it, so no driver can claim another
        // volume's identity.
        let recognised =
            recognise_extent(&mut disk, request).filter(|volume| volume.fstype == request.fstype);
        // A re-insert of a surprise-removed volume is recovered in place
        // — proven unmutated and replayed, or held read-only with its
        // retained set — never re-attached as a duplicate (D4c).
        if let Some((target, _claim)) = self
            .claim_reinsert_target(recognised.as_ref())
            .map_err(|errno| refused("recovery_unclaimed", errno))?
        {
            return self.recover(request, publisher, wiring, &device, &target);
        }
        // A name another volume holds is answered before the volume is judged,
        // so the volume manager tries its next candidate at once.
        let path = storage_path(name).map_err(|(cause, errno)| refused(cause, errno))?;
        if vfs.mounts().resolve(&path).path() == &path || self.name_reserved(name) {
            return Err(Self::name_taken(audit, name));
        }
        let Some(probed) = recognised else {
            return Err(refused("volume_unrecognised", Errno::BadMagic));
        };
        let publisher =
            publisher.ok_or_else(|| refused("no_publisher", Errno::PermissionDenied))?;
        // A volume whose identity another root publishes is attached already,
        // under another name: that is no name for the volume manager to try
        // its next candidate over.
        let unpublishable = if probed.identity == [0; 16] {
            Some(Errno::OutOfRange)
        } else {
            VOLUME_FOREST.resolve(&probed.identity).map(|_| Errno::Busy)
        };
        if let Some(errno) = unpublishable {
            return Err(refused("identity_unpublishable", errno));
        }

        // Thread the device through the uncommitted-write journal, its
        // mutation-evidence shadow seeded from the window the volume declares.
        let journal = seeded_journal(&mut disk, request, block_size, probed.evidence)
            .ok_or_else(|| refused("journal_unallocated", Errno::OutOfMemory))?;
        let journaled = JournaledBlock::new(disk, Arc::clone(&journal), wiring.pressure);
        let window = PartitionBlock::new(journaled, request.first_lba, request.blocks)
            .map_err(|err| refused("window_invalid", err.as_errno()))?;
        let handle = DriverHandle::from_raw(self.next_handle.fetch_add(1, Ordering::Relaxed))
            .map_err(|_| refused("handle_invalid", Errno::OutOfRange))?;
        let served = self
            .new_served(window, device.read_only)
            .map_err(|err| refused("served_volume_unbuilt", err))?;
        let volume = AttachedVolume {
            id: probed.identity,
            name: String::from(name),
            path,
            handle,
            endpoint: request.endpoint,
            window: request.window,
            journal,
            fstype: request.fstype,
            mount: Mount::Pending,
            served: Some(Arc::clone(&served)),
            node: None,
            first_lba: request.first_lba,
            blocks: request.blocks,
            availability: Availability::Available,
            claimed: true,
        };
        // Released as the attach returns, when a surprise removal that
        // landed while the volume was built is applied.
        let _claim = self
            .enter(volume)
            .map_err(|(cause, errno)| refused(cause, errno))?;
        let node = publish_volume_node(wiring.tree, publisher, request.fstype, served.id())
            .map_err(|err| {
                self.retract(&probed.identity);
                refused("volume_node_unpublished", err)
            })?;
        self.set_node(&probed.identity, Some((publisher, node)));
        RuntimeVolumeService::audit_event(
            audit,
            VOLUME_AWAITING_DRIVER,
            "volume-service: runtime volume attached; awaiting its filesystem driver",
            name,
            None,
        );
        Ok(())
    }

    fn detach(&self, request: &VolumeDetachRequest, caller: &Principal<'_>) -> Result<(), Errno> {
        let wiring = self.wiring()?;
        if self.network.holds(&request.volume_id) {
            return self.network.detach(wiring, request, caller);
        }
        if !caller.may_mount() {
            let name = self
                .attached_device(&request.volume_id)
                .map(|(name, _)| name);
            let Some(name) = name else {
                return Err(Errno::NotFound);
            };
            return Err(Self::refuse_detach(
                wiring.audit,
                &name,
                "no_mount_authority",
                Errno::PermissionDenied,
            ));
        }
        self.detach_volume(&request.volume_id, request.force, wiring.audit)
    }

    fn read(
        &self,
        volume: u64,
        lba: u64,
        len: usize,
        deliver: &mut ChunkSink<'_>,
    ) -> Result<(), Errno> {
        self.wiring()?;
        self.served(volume)?.read(lba, len, deliver)
    }

    fn write(
        &self,
        volume: u64,
        lba: u64,
        len: usize,
        fill: &mut ChunkSource<'_>,
    ) -> Result<(), Errno> {
        self.wiring()?;
        self.served(volume)?.write(lba, len, fill)
    }

    fn ctl(&self, volume: u64, ctl: &VolumeCtl) -> Result<Option<VolumeInfo>, Errno> {
        self.wiring()?;
        self.served(volume)?.ctl(ctl)
    }

    fn awaiting_server(&self, volume: u64) -> Result<(), Errno> {
        self.wiring()?;
        let state = self.state.lock();
        let entry = state
            .iter()
            .find(|v| v.granted_as(volume))
            .ok_or(Errno::NotFound)?;
        match entry.mount.server() {
            None => Ok(()),
            Some(_) => Err(Errno::AlreadyExists),
        }
    }

    fn serve(&self, volume: u64, offer: &ServeOffer, channel: ServeChannel) -> Result<(), Errno> {
        let wiring = self.wiring()?;
        // Taken in turn with the attaches, recoveries and detaches on the
        // volume's device, so a serve never races one.
        let (endpoint, window) = self.served_device(volume)?;
        let pin = self.pin_device(endpoint, window)?;
        let _op = pin.op.lock();
        let (_claim, target) = self.claim_awaiting(volume, (endpoint, window))?;
        let device = self
            .device_for(endpoint, window)
            .ok_or(Errno::DeviceOffline)?;
        let server = channel.endpoint.id().0;
        if let Err((cause, errno)) = Self::mount_served(&target, offer, channel, &device, wiring) {
            self.retract_unserved(&target, wiring.audit, cause, errno);
            return Err(errno);
        }
        if let Some(entry) = self.state.lock().iter_mut().find(|v| v.id == target.id) {
            entry.mount = Mount::Mounted {
                format: offer.format,
                server: Some(server),
            };
        }
        let (event, message) = if target.mounted_as.is_some() {
            (
                VOLUME_SERVE_RESUMED,
                "volume-service: a filesystem driver serves the volume again",
            )
        } else {
            (
                VOLUME_ATTACHED,
                "volume-service: runtime volume served, mounted and published",
            )
        };
        Self::audit_event(wiring.audit, event, message, &target.name, None);
        if !target.served.read_only() {
            Self::audit_write_refusal(wiring.audit, &target.name, offer.write_refusal);
        }
        Ok(())
    }

    fn refuse(&self, volume: u64, why: DriverError) -> Result<(), Errno> {
        let wiring = self.wiring()?;
        let (endpoint, window) = self.served_device(volume)?;
        let pin = self.pin_device(endpoint, window)?;
        let _op = pin.op.lock();
        let (_claim, target) = self.claim_awaiting(volume, (endpoint, window))?;
        self.retract_unserved(&target, wiring.audit, "driver_refused", why.as_errno());
        Ok(())
    }

    fn offer(
        &self,
        server: &OfferingServer<'_>,
        offer: &NetworkOffer<'_>,
        id: OfferId,
        channel: ServeChannel,
    ) -> Result<(), Errno> {
        let wiring = self.wiring()?;
        self.network.offer(wiring, server, offer, id, channel)
    }

    fn attach_offered(
        &self,
        caller: &Principal<'_>,
        offer: OfferId,
        attach: &NetworkAttach<'_>,
    ) -> Result<AttachReply, Errno> {
        let wiring = self.wiring()?;
        self.network.attach(wiring, caller, offer, attach, &|name| {
            self.name_reserved(name)
        })
    }

    fn offer_ctl(&self, server: u64, offer: OfferId, op: FsserveCtl) -> Result<(), Errno> {
        let wiring = self.wiring()?;
        self.network.ctl(wiring, server, offer, op)
    }
}

impl RuntimeVolumeService {
    /// Detach the volume `id`; when `force`, revoke the descriptors holding
    /// its nodes, withdraw the binds standing within it, and discard what
    /// cannot be committed.
    ///
    /// # Errors
    ///
    /// As [`VolumeService::detach`]: a plain detach is refused while a
    /// descriptor holds a node or a bind stands within the volume.
    fn detach_volume(
        &self,
        id: &[u8; 16],
        force: bool,
        audit: &'static (dyn Sink + Sync),
    ) -> Result<(), Errno> {
        let refuse = |name: &str, cause: &'static str, errno: Errno| -> Errno {
            Self::refuse_detach(audit, name, cause, errno)
        };
        self.retire_unused_devices();
        let Some((named, device)) = self.attached_device(id) else {
            return Err(refuse("unknown", "identity_not_attached", Errno::NotFound));
        };
        // A volume still on its device is detached under that device's lock,
        // so its final commit takes the disk's one client in turn with the
        // other volumes on it; one whose device is gone has nothing to commit
        // to.
        let pin = match device {
            Some((endpoint, window)) => Some(
                self.pin_device(endpoint, window)
                    .map_err(|errno| refuse(&named, "registry_full", errno))?,
            ),
            None => None,
        };
        let _op = pin.as_ref().map(|pin| pin.op.lock());
        let (_claim, target) = match self.claim_for_detach(id, device) {
            DetachClaim::Held(claim, target) => (claim, target),
            // Detached, or retracted by a surprise removal, while this waited:
            // the goal state is reached.
            DetachClaim::Gone => return Ok(()),
            DetachClaim::Busy(name) => return Err(refuse(&name, "volume_busy", Errno::Busy)),
        };
        let DetachTarget {
            name,
            handle,
            endpoint,
            window,
            journal,
            availability,
            served,
        } = target;
        let refused = |cause: &'static str, errno: Errno| -> Errno { refuse(&name, cause, errno) };
        let vfs = LATE_FILESYSTEM
            .vfs()
            .map_err(|err| refused("no_mount_table", err))?;
        // The table's own word, not the registry's: a reattach the heap
        // refused leaves a volume once mounted out of it, still detachable.
        let mounted = vfs.mounts().attachment(handle).is_some();
        let binds = if force {
            BindsWithin::Withdraw
        } else {
            BindsWithin::Refuse
        };
        // Asked before anything is revoked or discarded, so a volume the table
        // would keep loses nothing to a detach that cannot happen.
        if mounted {
            vfs.mounts()
                .detachable(handle, binds)
                .map_err(|err| refused("not_detachable", err.to_errno()))?;
        }

        // Held from the count of the volume's open nodes to the retraction:
        // no pin is taken meanwhile, no write lands between the final commit
        // and the retraction, and an operation that resolved the volume
        // before it left finds its projection gone, or reaches the
        // fail-closed stand-in instead of a driver whose writes would be
        // acknowledged and then dropped with it.
        let driver = LATE_FILESYSTEM.driver(handle).ok();
        let nodes = LATE_FILESYSTEM.nodes(handle);
        let mut fs = driver.as_deref().map(DriverLock::lock);
        let open = settle_holds(nodes.as_deref(), fs.as_deref_mut(), force);
        if open != 0 && !force {
            return Err(Self::refuse_open_nodes(audit, &name, open));
        }

        // A surprise-removed volume never detaches implicitly: discarding
        // its retained (or already lost) uncommitted data is the
        // deliberate, separately-audited force-unmount operation. A force
        // detach still commits a healthy volume cleanly when it can; only
        // when nothing can be committed does it discard.
        let commit_refusal = match availability {
            Availability::Available => {
                // A volume no driver serves has no filesystem state to flush:
                // what a lost driver held unpublished went with it.
                let live = if served {
                    fs.as_deref_mut()
                        .map(Some)
                        .ok_or(("driver_missing", Errno::NotImplemented))
                } else {
                    Ok(None)
                };
                live.and_then(|live| {
                    commit_for_detach(live, self.device_for(endpoint, window).as_ref(), &journal)
                })
                .err()
            }
            Availability::UnavailableDirty => {
                Some(("volume_unavailable_dirty", Errno::DeviceFault))
            }
            Availability::UnavailableLost => Some(("volume_unavailable_lost", Errno::DeviceFault)),
            // A conflicted volume's device is live, but its journal still
            // holds the retained set: a plain detach would discard it
            // silently, so only the audited force is the exit.
            Availability::RecoveryConflict => Some(("volume_recovery_conflict", Errno::NotEmpty)),
        };
        let discarded = match commit_refusal {
            None => None,
            Some((cause, errno)) => {
                if !force {
                    return Err(refused(cause, errno));
                }
                // The audited force-discard: read what is being given up
                // before wiping it, so the loss is never silent. A lost
                // journal reports zero retained bytes — that loss was
                // already on record at the surprise-removal transition.
                let mut journal = journal.lock();
                let bytes = journal.retained_bytes();
                journal.discard_all();
                Some((cause, bytes))
            }
        };

        // Retract, still under the driver's lock: the projections leave the
        // table and the stand-in takes the driver's place, so a held
        // descriptor answers stale; then the volume leaves the registry, the
        // tree and the namespace. The claim kept a surprise removal off the
        // entry, and the disk's client closes once no operation holds its
        // slot.
        if mounted {
            vfs.mounts_write()
                .detach(handle, binds)
                .map_err(|err| refused("not_detachable", err.to_errno()))?;
        }
        if let Some(fs) = fs.as_deref_mut() {
            *fs = Box::new(UnavailableFs);
        }
        self.retract(id);
        drop(fs);
        audit_detach_outcome(audit, &name, Forced::of(discarded, open));
        Ok(())
    }
}

/// What a force detach gave up beyond a plain one: the open nodes it revoked
/// and the retained writes it discarded.
struct Forced {
    /// Why a plain detach was refused.
    cause: &'static str,
    discarded_bytes: u64,
    revoked: usize,
}

impl Forced {
    /// What a detach that discarded `discarded` — the refused commit's cause
    /// and the bytes given up — and revoked `revoked` open nodes gave up, or
    /// `None` for one that gave up nothing.
    fn of(discarded: Option<(&'static str, u64)>, revoked: usize) -> Option<Self> {
        match (discarded, revoked) {
            (None, 0) => None,
            (None, revoked) => Some(Self {
                cause: "nodes_open",
                discarded_bytes: 0,
                revoked,
            }),
            (Some((cause, discarded_bytes)), revoked) => Some(Self {
                cause,
                discarded_bytes,
                revoked,
            }),
        }
    }
}

/// Audit a completed detach: the clean withdrawal, or the force-unmount and
/// what it gave up.
fn audit_detach_outcome(audit: &'static (dyn Sink + Sync), name: &str, forced: Option<Forced>) {
    let Some(forced) = forced else {
        RuntimeVolumeService::audit_event(
            audit,
            VOLUME_DETACHED,
            "volume-service: runtime volume flushed, unmounted, and withdrawn",
            name,
            None,
        );
        return;
    };
    log(
        audit,
        &Event {
            level: Level::Warn,
            id: VOLUME_FORCE_UNMOUNTED,
            message: "volume-service: volume force-unmounted over what a plain detach refuses",
            fields: &[
                Field {
                    key: "volume",
                    value: FieldValue::Str(name),
                },
                Field {
                    key: "cause",
                    value: FieldValue::Str(forced.cause),
                },
                Field {
                    key: "discarded_bytes",
                    value: FieldValue::UnsignedInt(forced.discarded_bytes),
                },
                Field {
                    key: "revoked_nodes",
                    value: FieldValue::UnsignedInt(forced.revoked as u64),
                },
            ],
        },
    );
}

/// How many descriptors hold pins of `nodes` once the releases waiting are
/// reaped into `fs`. When `force`, `fs` lets every hold go and the
/// registration is retired, so those descriptors answer stale.
fn settle_holds(
    nodes: Option<&VolumeNodes>,
    fs: Option<&mut Box<dyn KernelFs>>,
    force: bool,
) -> usize {
    let (Some(nodes), Some(fs)) = (nodes, fs) else {
        return 0;
    };
    nodes.reap(fs);
    let open = nodes.outstanding();
    if force {
        // A node the driver fails to free is left to the format's own
        // recovery, as after a crash.
        let _ = fs.release_pins();
        nodes.retire();
    }
    open
}

/// Commit everything a departing volume holds: flush the filesystem's own
/// state through `fs` when a driver serves it, its driver held by the
/// caller, then the device's cache, and empty the journal — after a
/// committed flush nothing is uncommitted. A vanished endpoint or window
/// (`NotFound` — the device was unplugged mid-detach) is tolerated only when
/// the journal is clean.
///
/// # Errors
///
/// `(cause, errno)` pairs the detach path audits through its refusal
/// helper; a refusal leaves the volume's state untouched (nothing is
/// discarded here — that is the caller's audited force path).
fn commit_for_detach(
    fs: Option<&mut Box<dyn KernelFs>>,
    device: Option<&SharedDevice>,
    journal: &Arc<SpinLock<RetainedWrites>>,
) -> Result<(), (&'static str, Errno)> {
    if let Some(fs) = fs {
        fs.flush()
            .map_err(|err| ("filesystem_flush_failed", err.as_errno()))?;
    }
    // The device cache is flushed through the volume's own shared window,
    // so the flush is serialised against the sibling volumes still driving
    // the same disk rather than racing them over the one staging buffer.
    //
    // A device that vanished mid-detach cannot be committed to at all, which
    // is tolerable only when nothing was uncommitted. That is asked of the
    // endpoint registry rather than inferred from the failure class, so a
    // genuine flush failure on a *live* device is never mistaken for a
    // vanished one and quietly tolerated.
    let Some(device) = device else {
        return vanished_commit(journal);
    };
    match device.window().flush() {
        Ok(()) | Err(DriverError::Unsupported) => {
            journal.lock().commit();
            Ok(())
        }
        // A stale refusal is the driver's word that the publication this
        // volume was attached over is gone: its medium left.
        Err(DriverError::Stale) => vanished_commit(journal),
        Err(err) => {
            if tairix_kernel_core::callreg::contains(EndpointId(device.endpoint)) {
                Err(("device_flush_failed", err.as_errno()))
            } else {
                vanished_commit(journal)
            }
        }
    }
}

/// The commit verdict for a volume whose device vanished before its cache
/// could be flushed: tolerated when nothing was uncommitted, refused when
/// the journal still holds writes the medium never took.
fn vanished_commit(journal: &Arc<SpinLock<RetainedWrites>>) -> Result<(), (&'static str, Errno)> {
    if journal.lock().is_dirty() {
        Err(("device_vanished_dirty", Errno::DeviceFault))
    } else {
        Ok(())
    }
}

/// The facts of an unavailable volume a re-insert may recover
/// (`plans/DEVICES.md` D4c), snapshotted out of the registry so the
/// device I/O below never runs under the registry guard.
struct RecoveryTarget {
    /// The catalog name the volume keeps through recovery.
    name: String,
    /// The existing mount-table path, remounted in place.
    path: Path,
    /// The existing registry handle, re-pointed at the live driver.
    handle: DriverHandle,
    /// What the mount table calls the volume, its stand-in's included.
    format: FormatName,
    /// The published 16-byte identity the re-insert matched.
    id: [u8; 16],
    /// The journal holding the retained set and the evidence shadow.
    journal: Arc<SpinLock<RetainedWrites>>,
    /// The original extent — the frame of reference the retained LBAs
    /// live in; replay requires the identical extent.
    first_lba: u64,
    blocks: u64,
    /// Whether retention had been abandoned (the unavailable-lost state).
    lost: bool,
}

/// How a re-insert resolved: the retained writes replayed onto the
/// proven-unmutated medium, or the read-only conflict fallback with the
/// cause that forbade replay.
enum RecoveryOutcome {
    /// Non-mutation was proven; this many retained bytes were replayed
    /// and committed.
    Replayed(u64),
    /// Replay was refused for the named cause; the volume returns
    /// read-only with its retained set kept.
    Conflict(&'static str),
}

/// The applied surprise-removal transition [`RuntimeVolumeService::
/// handle_vanished`] reports on: the volume either retracted cleanly or
/// became unavailable with its journal's outcome on record.
enum VanishOutcome {
    /// Nothing was uncommitted: the whole entry retracts.
    Clean(AttachedVolume),
    /// Uncommitted writes exist: held, with their byte count, or lost.
    Unavailable {
        name: String,
        handle: DriverHandle,
        format: FormatName,
        id: [u8; 16],
        pending: Pending,
        served: Option<Arc<ServedVolume>>,
        node: Option<(u32, u32)>,
    },
}

/// What a surprise-removed volume had not committed to its medium.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Pending {
    /// Device writes the journal holds for a re-insert to replay.
    Retained(u64),
    /// Writes nothing holds any more, and why.
    Lost(&'static str),
}

impl RuntimeVolumeService {
    /// Claim the volume an attach's `recognised` extent re-inserts, if
    /// any: its identity and format must match an unavailable volume no
    /// other operation holds. Only a volume whose serving driver is gone is a recovery
    /// target: a live volume with the same identity is a duplicate, refused
    /// by the normal attach path.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the target cannot be held; the volume is
    /// left unclaimed.
    fn claim_reinsert_target(
        &self,
        recognised: Option<&ProbedVolume>,
    ) -> Result<Option<(RecoveryTarget, VolumeClaim<'_>)>, Errno> {
        let Some(recognised) = recognised else {
            return Ok(None);
        };
        let identity = recognised.identity;
        let mut state = self.state.lock();
        let found = state.iter_mut().find(|v| {
            v.id == identity
                && v.fstype == recognised.fstype
                && !v.claimed
                && matches!(
                    v.availability,
                    Availability::UnavailableDirty | Availability::UnavailableLost
                )
        });
        let Some(volume) = found else {
            return Ok(None);
        };
        let Some(format) = volume.mount.format() else {
            return Ok(None);
        };
        let name = concatenated(&[&volume.name]).ok_or(Errno::OutOfMemory)?;
        let path = volume.path.owned().map_err(VfsError::to_errno)?;
        volume.claimed = true;
        let target = RecoveryTarget {
            name,
            path,
            handle: volume.handle,
            format,
            id: volume.id,
            journal: Arc::clone(&volume.journal),
            first_lba: volume.first_lba,
            blocks: volume.blocks,
            lost: volume.availability == Availability::UnavailableLost,
        };
        Ok(Some((
            target,
            VolumeClaim {
                service: self,
                id: identity,
            },
        )))
    }

    /// Prove the re-inserted medium was not mutated elsewhere and replay
    /// the retained writes onto it, or name the conflict that forbids
    /// replay (`plans/DEVICES.md` D4c). Any doubt is a conflict — never a
    /// silent merge.
    fn attempt_replay<B: Block>(
        request: &VolumeAttachRequest<'_>,
        client: &mut B,
        block_size: u32,
        target: &RecoveryTarget,
        device_read_only: bool,
    ) -> RecoveryOutcome {
        // Nothing retained can be replayed after retention was abandoned.
        if target.lost || target.journal.lock().is_lost() {
            return RecoveryOutcome::Conflict("retention_lost");
        }
        // The retained LBAs live in the original extent's frame of
        // reference; a moved or resized extent can never accept them.
        if target.first_lba != request.first_lba || target.blocks != request.blocks {
            return RecoveryOutcome::Conflict("extent_changed");
        }
        // The retained writes cannot land on a now write-protected medium.
        if device_read_only {
            return RecoveryOutcome::Conflict("device_read_only");
        }
        let Some((evidence_lba, evidence_len)) = target.journal.lock().evidence_window() else {
            return RecoveryOutcome::Conflict("no_evidence");
        };
        let extent_end = request.first_lba.saturating_add(request.blocks);
        let evidence_end = u64::try_from(evidence_len)
            .ok()
            .map(|len| len.div_ceil(u64::from(block_size.max(1))))
            .and_then(|blocks| evidence_lba.checked_add(blocks));
        if evidence_lba < request.first_lba || evidence_end.is_none_or(|end| end > extent_end) {
            return RecoveryOutcome::Conflict("no_evidence");
        }
        let Some(current) = read_blocks_at(client, evidence_lba, evidence_len) else {
            return RecoveryOutcome::Conflict("evidence_unreadable");
        };
        if !target.journal.lock().verify_evidence(&current) {
            return RecoveryOutcome::Conflict("evidence_mismatch");
        }
        // Proven unmutated: replay the retained set, commit it to the
        // medium, and empty the journal. A failure mid-replay falls back
        // to the conflict path with the set still held (the journal is
        // only committed after the device confirms the flush).
        let Some(snapshot) = target.journal.lock().retained_snapshot() else {
            return RecoveryOutcome::Conflict("replay_failed");
        };
        let mut replayed = 0u64;
        for (lba, data) in snapshot.blocks() {
            if client.write_blocks(*lba, data).is_err() {
                return RecoveryOutcome::Conflict("replay_failed");
            }
            replayed += data.len() as u64;
        }
        match client.flush() {
            Ok(()) | Err(DriverError::Unsupported) => {}
            Err(_) => return RecoveryOutcome::Conflict("replay_failed"),
        }
        target.journal.lock().commit();
        RecoveryOutcome::Replayed(replayed)
    }

    /// Recover the unavailable volume `target` over the re-inserted
    /// device: verify and replay ([`Self::attempt_replay`]), then serve the
    /// volume in place — same handle, mount path, catalog name, and
    /// published root — over a fresh served volume and node, writable when
    /// proven and read-only with the retained set kept when not. The
    /// stand-in answers the mount until the new driver serves it. The caller
    /// holds the re-inserted device's operation lock and the claim on
    /// `target`, whose release applies a surprise removal that landed while
    /// the volume was rebuilt.
    fn recover(
        &self,
        request: &VolumeAttachRequest<'_>,
        publisher: Option<u32>,
        wiring: &Wiring,
        device: &SharedDevice,
        target: &RecoveryTarget,
    ) -> Result<(), Errno> {
        let audit = wiring.audit;
        let refused = |cause, errno| Self::refuse_attach(audit, &target.name, cause, errno);
        let publisher =
            publisher.ok_or_else(|| refused("no_publisher", Errno::PermissionDenied))?;
        let vfs = LATE_FILESYSTEM
            .vfs()
            .map_err(|err| refused("no_mount_table", err))?;
        let mut disk = device.window();
        let outcome = Self::attempt_replay(
            request,
            &mut disk,
            device.block_size(),
            target,
            device.read_only,
        );
        let conflict = matches!(outcome, RecoveryOutcome::Conflict(_));
        // A conflicted volume returns read-only until its retained set is
        // explicitly discarded, down to the blocks its driver may write.
        let read_only = device.read_only || conflict;
        let journaled = JournaledBlock::new(disk, Arc::clone(&target.journal), wiring.pressure);
        let window = PartitionBlock::new(journaled, request.first_lba, request.blocks)
            .map_err(|err| refused("window_invalid", err.as_errno()))?;
        let path = held_path(&target.path).map_err(|(cause, errno)| refused(cause, errno))?;
        let served = self
            .new_served(window, read_only)
            .map_err(|err| refused("served_volume_unbuilt", err))?;
        // Under the stand-in's lock, so an operation that takes it after the
        // move finds its projection gone. The stand-in answers under a fresh
        // registration: a descriptor held across the removal answers stale,
        // and holds up no detach.
        let standing = LATE_FILESYSTEM.driver(target.handle).ok();
        let held = standing.as_deref().map(DriverLock::lock);
        reattach(
            vfs,
            Attachment::new(target.handle, device.medium, mount_flags(read_only)),
            path,
            mount_template(identity_map_gid(request.fstype)),
        )
        .map_err(|(cause, errno)| {
            self.retire_served(Some(&served), None);
            refused(cause, errno)
        })?;
        let _ = LATE_FILESYSTEM.unregister(target.handle);
        let _ = LATE_FILESYSTEM.register(
            target.handle,
            Box::new(UnavailableFs),
            &target.name,
            target.format.as_str(),
            target.id,
            None,
        );
        drop(held);
        let _ = LATE_FILESYSTEM.set_io_source(target.handle, device.health.clone());
        let _ = LATE_FILESYSTEM.set_availability(
            target.handle,
            if conflict {
                MountAvailability::RecoveryConflict
            } else {
                MountAvailability::Recovering
            },
        );
        if let Some(entry) = self.state.lock().iter_mut().find(|v| v.id == target.id) {
            entry.endpoint = request.endpoint;
            entry.window = request.window;
            entry.served = Some(Arc::clone(&served));
            entry.availability = if conflict {
                Availability::RecoveryConflict
            } else {
                Availability::Available
            };
        }
        audit_recovery_outcome(audit, &target.name, &outcome, &target.journal);
        let node = publish_volume_node(wiring.tree, publisher, request.fstype, served.id())
            .map_err(|err| refused("volume_node_unpublished", err))?;
        self.set_node(&target.id, Some((publisher, node)));
        Ok(())
    }

    /// Decide and apply the state transition for a volume `removal` took
    /// from `endpoint`, under the registry guard: a clean volume leaves the
    /// registry, a dirty/lost one is marked unavailable. `None` when no
    /// available volume matches (idempotency). A claimed volume is left to
    /// the operation holding it, which applies the transition as it releases
    /// the claim.
    fn vanish_outcome(&self, endpoint: u64, removal: Removal) -> Option<VanishOutcome> {
        let mut state = self.state.lock();
        // A conflicted volume is served by a live driver too: when that
        // driver dies its cache-bearing stand-in must be replaced and the
        // (still-dirty) journal re-decides the unavailable state.
        let index = {
            let devices = self.devices.lock();
            state.iter().position(|v| {
                v.endpoint == endpoint
                    && v.availability.device_present()
                    && !v.claimed
                    && (removal == Removal::Endpoint
                        || slot_retracted(&devices, v.endpoint, v.window))
            })?
        };
        // A volume no driver ever served holds nothing a caller was told was
        // written.
        let Some(format) = state[index].mount.format() else {
            return Some(VanishOutcome::Clean(state.remove(index)));
        };
        // Writes the driver acknowledged but never gave the device are in no
        // journal, and go with the driver: a removal holding some is a loss.
        let unpublished = LATE_FILESYSTEM.holds_unpublished(state[index].handle);
        let pending = {
            let journal = state[index].journal.lock();
            if journal.is_lost() {
                Some(Pending::Lost("retention_abandoned"))
            } else if unpublished {
                Some(Pending::Lost("unpublished_writes"))
            } else {
                journal
                    .is_dirty()
                    .then(|| Pending::Retained(journal.retained_bytes()))
            }
        };
        let Some(pending) = pending else {
            return Some(VanishOutcome::Clean(state.remove(index)));
        };
        let entry = &mut state[index];
        entry.availability = match pending {
            Pending::Retained(_) => Availability::UnavailableDirty,
            Pending::Lost(_) => Availability::UnavailableLost,
        };
        entry.mount = Mount::Mounted {
            format,
            server: None,
        };
        Some(VanishOutcome::Unavailable {
            name: entry.name.clone(),
            handle: entry.handle,
            format,
            id: entry.id,
            pending,
            served: entry.served.take(),
            node: entry.node.take(),
        })
    }

    /// The surprise-removal transition for every volume `removal` took from
    /// the block-service endpoint `endpoint` (`plans/DEVICES.md` D4) — a
    /// partitioned stick carries several: clean volumes retract, dirty ones
    /// become unavailable with their retained set held, lost ones become
    /// unavailable with the loss on record. Idempotent — a volume already
    /// transitioned (or detached) is left alone. Takes no sleeping lock, so
    /// it is safe from the endpoint-teardown context.
    fn handle_vanished(&self, endpoint: u64, removal: Removal) {
        let Ok(wiring) = self.wiring() else {
            return;
        };
        while let Some(outcome) = self.vanish_outcome(endpoint, removal) {
            self.apply_vanish(outcome, wiring.audit);
        }
        self.release_vanished_cache(endpoint, removal);
    }

    /// Drop what the vanished devices' caches hold, so a removed medium's
    /// blocks do not outlive it in kernel memory. Only a cache nothing is
    /// using this instant can be reached without waiting; one that is goes
    /// with its device at the next attach or detach.
    fn release_vanished_cache(&self, endpoint: u64, removal: Removal) {
        let vanished: Vec<SharedDevice> = self
            .devices
            .lock()
            .iter()
            .filter(|slot| {
                slot.endpoint == endpoint && (removal == Removal::Endpoint || slot.retracted)
            })
            .filter_map(|slot| slot.connected.clone())
            .collect();
        for device in &vanished {
            device.device.try_with_device(BlockCache::drop_all);
        }
    }

    /// Carry out one volume's surprise-removal `outcome`.
    fn apply_vanish(&self, outcome: VanishOutcome, audit: &'static (dyn Sink + Sync)) {
        match outcome {
            VanishOutcome::Clean(entry) => {
                // No sleeping lock may be taken here, so the registration is
                // only retired: its pins go with the driver, and a deletion
                // they deferred is the format's own recovery's to finish.
                self.withdraw(&entry);
                RuntimeVolumeService::audit_event(
                    audit,
                    VOLUME_SURPRISE_REMOVED_CLEAN,
                    "volume-service: surprise removal with nothing uncommitted; volume retracted",
                    &entry.name,
                    None,
                );
            }
            VanishOutcome::Unavailable {
                name,
                handle,
                format,
                id,
                pending,
                served,
                node,
            } => {
                // Its driver goes with the device. The fail-closed stand-in
                // takes over the volume's registration in place: the proxy
                // (and its plaintext cache) drops, so nothing of the vanished
                // medium is ever served as live, and a descriptor held across
                // the removal answers the device fault; the mount and the
                // published root stay visible — and the registry entry is
                // marked unavailable, so the mount snapshot never shows the
                // vanished volume as healthy.
                self.retire_served(served.as_deref(), node);
                Self::stand_in(handle, &name, format, id);
                let _ = LATE_FILESYSTEM.set_availability(
                    handle,
                    match pending {
                        Pending::Retained(_) => MountAvailability::UnavailableDirty,
                        Pending::Lost(_) => MountAvailability::UnavailableLost,
                    },
                );
                match pending {
                    Pending::Retained(retained) => {
                        log(
                            audit,
                            &Event {
                                level: Level::Warn,
                                id: VOLUME_SURPRISE_REMOVED_DIRTY,
                                message: "volume-service: surprise removal with uncommitted \
                                          writes retained; volume unavailable-dirty",
                                fields: &[
                                    Field {
                                        key: "volume",
                                        value: FieldValue::Str(&name),
                                    },
                                    Field {
                                        key: "retained_bytes",
                                        value: FieldValue::UnsignedInt(retained),
                                    },
                                ],
                            },
                        );
                    }
                    Pending::Lost(cause) => RuntimeVolumeService::audit_event(
                        audit,
                        VOLUME_SURPRISE_REMOVED_LOST,
                        "volume-service: surprise removal lost uncommitted data; volume \
                         unavailable-lost",
                        &name,
                        Some(cause),
                    ),
                }
            }
        }
    }
}

impl RuntimeVolumeService {
    /// Put the fail-closed stand-in in the place of the driver registered for
    /// `handle`, under its registration, so a descriptor opened through it
    /// answers the device fault rather than going stale, and no operation
    /// finds the handle unregistered in between. Parks on nothing.
    fn stand_in(handle: DriverHandle, name: &str, format: FormatName, id: [u8; 16]) {
        if let Some(nodes) = LATE_FILESYSTEM.nodes(handle) {
            let _ = LATE_FILESYSTEM.register_with_nodes(
                handle,
                Box::new(UnavailableFs),
                name,
                format.as_str(),
                id,
                None,
                nodes,
            );
        }
    }

    /// The filesystem driver serving over `server` is gone: the fail-closed
    /// stand-in answers its volume, which reports itself recovering until the
    /// driver `devmgr` restarts serves it again. Takes no sleeping lock, like
    /// the surprise-removal path.
    fn server_lost(&self, server: u64) {
        let Ok(wiring) = self.wiring() else {
            return;
        };
        self.network.server_lost(server, wiring.audit);
        let lost = {
            let mut state = self.state.lock();
            state
                .iter_mut()
                .find(|v| v.mount.server() == Some(server) && !v.claimed)
                .and_then(|v| {
                    let format = v.mount.format()?;
                    v.mount = Mount::Mounted {
                        format,
                        server: None,
                    };
                    let conflicted = v.availability == Availability::RecoveryConflict;
                    Some((
                        v.name.clone(),
                        v.handle,
                        format,
                        v.id,
                        conflicted,
                        (v.endpoint, v.window),
                    ))
                })
        };
        let Some((name, handle, format, id, conflicted, (endpoint, window))) = lost else {
            return;
        };
        let unpublished = LATE_FILESYSTEM.holds_unpublished(handle);
        Self::stand_in(handle, &name, format, id);
        if let Some(device) = self.device_for(endpoint, window) {
            let _ = LATE_FILESYSTEM.set_io_source(handle, device.health);
        }
        let _ = LATE_FILESYSTEM.set_availability(
            handle,
            if conflicted {
                MountAvailability::RecoveryConflict
            } else {
                MountAvailability::Recovering
            },
        );
        Self::audit_event(
            wiring.audit,
            VOLUME_SERVER_LOST,
            "volume-service: the volume's filesystem driver is gone; awaiting its restart",
            &name,
            Some(if unpublished {
                "unpublished_writes_lost"
            } else {
                "nothing_unpublished"
            }),
        );
    }
}

impl EndpointVanishObserver for RuntimeVolumeService {
    fn endpoint_vanished(&self, id: EndpointId) {
        if is_served_endpoint(id.0) {
            self.server_lost(id.0);
        } else {
            self.handle_vanished(id.0, Removal::Endpoint);
        }
    }

    /// The medium left: the endpoint's device slots are marked retracted,
    /// so a volume an operation holds, or has yet to enter, is transitioned
    /// as that operation lets go, and the rest are transitioned now. The
    /// endpoint's current publication is the one retracting, so no slot of
    /// a later one exists yet.
    fn publication_retracted(&self, id: EndpointId) {
        for slot in self
            .devices
            .lock()
            .iter_mut()
            .filter(|slot| slot.endpoint == id.0)
        {
            slot.retracted = true;
        }
        self.handle_vanished(id.0, Removal::Publication);
    }
}

/// Whether the slot of `endpoint` and `window` among `devices` was retracted.
fn slot_retracted(devices: &[DeviceSlot], endpoint: u64, window: u64) -> bool {
    devices
        .iter()
        .any(|slot| slot.endpoint == endpoint && slot.window == window && slot.retracted)
}

#[cfg(test)]
#[path = "volume_service_tests.rs"]
mod tests;
