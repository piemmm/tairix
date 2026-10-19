//! Production root-volume mount + users-database load composition
//! (`plans/PI.md` §3 P11 root-mount increment).
//!
//! The one place that turns what a boot path finds on the storage device —
//! the root volume, plaintext or encrypted, and for an encrypted one the
//! plaintext `root.unlock` key-derivation descriptor `tools/mkimage` / the
//! installer plant on the FAT boot partition plus the passphrase the operator
//! types — into the validated `users-v1` database the `users_db_read` syscall
//! serves (`kernel/core::load_users_db_source`). It is the one layer
//! permitted to name both the `arxfs` driver and `kernel/core`
//! (`tairix-kernel`, `Layer::Tooling`).
//!
//! [`read_root_lock`] reads how the root is locked: the root volume's own
//! protection word, which must agree with the boot partition — a descriptor
//! beside a plaintext root, or an encrypted root with none, is a disk that is
//! not what it claims and is refused. [`mount_root_disk_and_load_users`] then
//! mounts it: a plaintext root with no key, an encrypted one under the key
//! [`UnlockDescriptor::derive_volume_key`] derives from the passphrase (held
//! [`Zeroizing`], so it is wiped on drop), where a wrong passphrase never
//! unwraps the master key and is refused with [`DriverError::PermissionDenied`]
//! — no oracle. [`load_users_db_source`] then reads and validates
//! `/System/Security/Users` under the kernel's capability-less `uid 0`
//! bootstrap identity.
//!
//! Every refusal is audited and yields **no** database, so a system whose root
//! cannot be mounted or whose database cannot be read serves none rather than
//! inventing accounts. The partition table is parsed by role (`FatBoot` /
//! `ARXFSRoot`, MBR or GPT), so one definition serves every board.

use alloc::boxed::Box;
use alloc::sync::Arc;

use tairix_abi::boot::MAX_ROOT_PASSPHRASE_LEN;
use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{FilesystemRead, FilesystemSecurity, NodeKind};
use tairix_abi::{DriverError, Errno};
use tairix_drv_fs_arxfs::{
    Keying, UnlockDescriptor, VolumeKey, VolumeProtection, ARXFS, ROOT_UNLOCK_NAME,
    UNLOCK_DESCRIPTOR_LEN,
};
use tairix_drv_fs_fat32::Fat32;
use tairix_kernel_core::boot_session::{LateBootSession, LATE_BOOT_SESSION};
use tairix_kernel_core::{
    build_identity_table, load_groups_db, load_users_db_source, read_bootstrap_file,
    read_image_system_config, BoundedReadError, ConsoleRead, ConsoleWrite, GroupsLoadError,
    HeldUsersDbSource, ImageConfig, LateGroupsDb, LateIdentity, LateUsersAdmin, LateUsersDb,
    SleepLock, UserAdminEngine, UsersDbSource, UsersLoadError, VfsError, GROUPS_DB, SYSTEM_CONFIG,
    USERS_DB,
};
use tairix_kernel_sec::{GroupId, IdentityTable};
use tairix_log::{log, Event, EventId, Field, Level, Sink};
use tairix_partition::{
    parse_partition_table, PartitionBlock, PartitionError, PartitionTable, PartitionType,
};
use tairix_supervisor::{
    run_supervisor, MountOutcome, Report as SupReport, RootLock as SupervisorRootLock, SupInput,
    SupervisorExit, SupervisorHost,
};
use tairix_users::{GroupsDb, UsersDb};

use crate::kernel_fs::KernelFs;
use crate::transform_cache::SharedClusterCache;
use crate::user_admin_backing::RootAdminBacking;
use crate::volume_policy::LateStorageGid;
use zeroize::{Zeroize, Zeroizing};

/// A mounted root volume, viewed as the read + security surface the
/// driver-store file service needs.
///
/// Blanket-implemented for every filesystem driver that is both
/// [`FilesystemRead`] and [`FilesystemSecurity`] (the production `ARXFS`
/// among them), so `&mut dyn RootVolume` is the **object-safe** handle the
/// continuation passed to [`with_system_volume`] receives — letting the
/// generic unlock policy hand a freshly mounted, concretely-typed volume to
/// a continuation without itself naming the concrete filesystem type. Its supertraits are exactly the bound the
/// [`SystemFileService`](crate::system_files::SystemFileService) the
/// driver-store server builds over the volume requires.
pub trait RootVolume: FilesystemRead + FilesystemSecurity {}

impl<T: FilesystemRead + FilesystemSecurity + ?Sized> RootVolume for T {}

/// Audit event: the root volume was mounted — with no key, or unlocked under
/// the passphrase-derived key — naming its `protection`. The subsequent
/// users-database read is audited separately by [`load_users_db_source`]
/// (`UsersDbLoaded` / `UsersDbRejected`).
const ROOT_MOUNT_UNLOCKED: EventId = EventId(4133);

/// Audit event: the root unlock was refused by a **structural** failure
/// before a database could be served — the on-FAT descriptor could not be
/// read or failed to decode, the partition table or a partition was
/// missing/malformed, the volume was not arxfs, or the device faulted.
/// The `cause` field names which check refused; no secret (passphrase,
/// key, or volume bytes) is ever logged. The
/// decision fails closed: no database is held.
///
/// A *wrong passphrase* is **not** one of these — that is an expected
/// authentication non-match recorded at [`ROOT_UNLOCK_KEY_REJECTED`], not
/// a system error.
const ROOT_MOUNT_REJECTED: EventId = EventId(4134);

/// Audit event: the passphrase-derived key did not unlock the encrypted root
/// volume — a wrong passphrase. An expected fail-closed authentication
/// non-match, not a system error, so it is recorded at [`Level::Debug`],
/// below the default [`Level::Info`] filter: routine retries cannot flood the
/// boot log, and the record stays available for brute-force forensics when
/// the level is lowered. No secret is ever logged.
const ROOT_UNLOCK_KEY_REJECTED: EventId = EventId(4142);

/// Audit event: the read-only, signed-bundle, plaintext `/System` volume (the
/// design-B pre-unlock driver store, `plans/PI.md`) was discovered and mounted
/// read-only over its `lib/partition` window.
const SYSTEM_VOLUME_MOUNTED: EventId = EventId(4140);

/// The message the `SYSTEM_VOLUME_MOUNTED` audit event carries — the single
/// source both the log site and an audit observer key on, so the two spellings
/// cannot drift. Reaching it proves the read-only `/System` root was
/// brought up over the disk, which on a real boot requires the disk's
/// completion interrupt to have woken the bring-up repeatedly (the x86_64
/// MSI-X device-IRQ path).
pub const SYSTEM_VOLUME_MOUNTED_MESSAGE: &str = "root-mount: read-only /System volume mounted";

/// Audit event: no read-only `/System` volume was mounted — the disk
/// carries no `ARXFSSystem` partition, or the volume's window could not be
/// built or opened read-only. Not fatal: the root still serves the system,
/// so the boot proceeds. The `cause` field names which check declined,
/// secret-free.
const SYSTEM_VOLUME_UNAVAILABLE: EventId = EventId(4141);

/// Why the root mount produced no users database.
///
/// Each variant carries the underlying error from the first check that
/// refused; the composition stops at the first failure and returns it
/// (fail closed, never partially applied).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RootMountError {
    /// The plaintext `root.unlock` descriptor could not be read off the
    /// FAT boot partition: the partition did not mount as FAT32, the
    /// [`ROOT_UNLOCK_NAME`] entry was absent ([`DriverError::NotFound`]),
    /// it was not a regular file of exactly [`UNLOCK_DESCRIPTOR_LEN`]
    /// bytes, or the device read faulted. The boot path recovers the
    /// descriptor before anything is decrypted, so a missing or malformed
    /// one yields no database rather than a fabricated default.
    DescriptorRead(DriverError),
    /// The on-FAT `root.unlock` descriptor failed to decode: bad magic,
    /// an unknown KDF id, an out-of-range iteration count, or a short
    /// buffer. The descriptor is plaintext and untrusted, so it is fully
    /// validated before its parameters drive any key derivation.
    Descriptor(DriverError),
    /// The root volume could not be probed or mounted:
    /// [`DriverError::PermissionDenied`] for a wrong passphrase (the master
    /// key never unwraps), [`DriverError::BadMagic`] for a volume that is not
    /// arxfs, [`DriverError::Unsupported`] for a protection it does not
    /// declare, or a device fault.
    Mount(DriverError),
    /// The boot partition carries an unlock descriptor but the root volume is
    /// plaintext: a disk that is not what it claims, refused rather than
    /// mounted without the protection its boot partition promises.
    DescriptorBesidePlaintextRoot,
    /// The volume mounted but `/System/Security/Users` could not be read
    /// or validated; [`load_users_db_source`] has already audited the
    /// precise cause.
    Users(UsersLoadError),
    /// The disk's partition table could not be parsed: no recognised
    /// scheme, a malformed or forged MBR/GPT table, or a device read
    /// fault. The table is untrusted on-disk input, validated fail-closed
    /// before any partition is trusted.
    PartitionTable(PartitionError),
    /// The disk carries no FAT boot partition (the partition holding the
    /// `root.unlock` descriptor), so its encrypted root cannot be unlocked.
    /// No database is served.
    NoBootPartition,
    /// The disk carries no `ARXFS` root partition to mount.
    NoRootPartition,
    /// A parsed partition extent does not fit the device geometry, so its
    /// bounds-checked window could not be built (a
    /// fixed extent, validated against the device before any access).
    PartitionWindow(DriverError),
    /// The volume mounted but `/System/Security/Groups` could not be read
    /// or validated; [`load_groups_db`] has already audited the precise
    /// cause. Without a group registry the kernel identity table cannot be
    /// built, so the unlock fails closed rather than resolving caller groups
    /// against an invented registry.
    Groups(GroupsLoadError),
    /// The user and group databases read but the authoritative identity
    /// table could not be built (a dangling group reference, a duplicate id,
    /// or — never on well-formed input — a re-parse of the validated users
    /// text failing). No identity is installed, so the `fs_*` path resolves
    /// no caller groups (fail closed).
    Identity(Errno),
}

impl RootMountError {
    /// Short, stable, secret-free cause string for the audit record.
    #[must_use]
    pub fn cause(self) -> &'static str {
        match self {
            Self::DescriptorRead(_) => "descriptor_unreadable",
            Self::Descriptor(_) => "descriptor_invalid",
            Self::Mount(DriverError::PermissionDenied) => "unlock_refused",
            Self::Mount(DriverError::BadMagic) => "not_a_arxfs_volume",
            Self::Mount(DriverError::Unsupported) => "volume_unsupported",
            Self::Mount(_) => "mount_failed",
            Self::DescriptorBesidePlaintextRoot => "descriptor_beside_plaintext_root",
            Self::Users(err) => err.cause(),
            Self::PartitionTable(_) => "partition_table_invalid",
            Self::NoBootPartition => "no_boot_partition",
            Self::NoRootPartition => "no_root_partition",
            Self::PartitionWindow(_) => "partition_out_of_range",
            Self::Groups(err) => err.cause(),
            Self::Identity(_) => "identity_build_failed",
        }
    }
}

/// The product of a successful root mount.
///
/// Carries both halves the trusted mount step publishes: the validated
/// `users-v1` text served to `users_db_read`, and the authoritative
/// user/group [`IdentityTable`] the userland `fs_*` path resolves a
/// caller's groups against. They are produced together — from the one
/// mounted root that holds both `/System/Security/Users` and
/// `/System/Security/Groups` — so a single trusted step installs both
/// (`PREREQUISITES.md` P-A).
pub struct UnlockedRoot {
    /// The validated `users-v1` text (zeroed on drop), installed into the
    /// [`LateUsersDb`] cell and served to `users_db_read`.
    pub users: HeldUsersDbSource,
    /// The verified user/group identity table, installed into the
    /// [`LateIdentity`] cell and consulted by the `fs_*` syscalls to resolve
    /// a caller's primary and supplementary groups kernel-side.
    pub identity: IdentityTable,
    /// The parsed user database, retained so the `CAP_USER_ADMIN` engine
    /// starts from exactly the verified boot state
    /// (`plans/CAPABILITY_USE.md` CU4).
    pub users_db: UsersDb,
    /// The parsed group registry, retained for the same engine.
    pub groups_db: GroupsDb,
    /// How the root volume opens, carried out of the mount so a
    /// [`WritableRootSink`] can open its read-write view and publish it as the
    /// writable `/System/Logs` + `/System/Settings` backing
    /// (`PREREQUISITES.md` P-B). An encrypted root's key is the most sensitive
    /// value the mount produces: it lives no longer than the publish step and
    /// is wiped the instant this struct drops.
    pub keying: RootKeying,
}

/// How a mounted root volume opens again: with no key, or under the key its
/// passphrase derived, zeroed on drop.
pub enum RootKeying {
    /// A plaintext root.
    Plaintext,
    /// An encrypted root's volume key.
    Key(Zeroizing<VolumeKey>),
}

impl RootKeying {
    /// The keying an open of the root volume takes.
    #[must_use]
    pub fn keying(&self) -> Keying<'_> {
        match self {
            Self::Plaintext => Keying::Plaintext,
            Self::Key(key) => Keying::Key(key),
        }
    }
}

/// How a disk's root volume is locked, as [`probe_root_lock`] finds it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RootLock {
    /// A plaintext root, mounted with no passphrase.
    Plaintext,
    /// An encrypted root, unlocked by a passphrase derived through this
    /// descriptor.
    Passphrase(UnlockDescriptor),
}

/// What [`mount_root_disk_and_load_users`] mounts the root with.
#[derive(Copy, Clone)]
pub enum RootUnlock<'a> {
    /// No key: the root is plaintext.
    Plaintext,
    /// The passphrase an encrypted root's key is derived from, through its
    /// descriptor. Used only to derive the key, never logged or retained.
    Passphrase {
        /// The root's unlock descriptor.
        descriptor: &'a UnlockDescriptor,
        /// The typed passphrase.
        passphrase: &'a [u8],
    },
}

/// A seam the interactive unlock calls, on success, to publish a writable
/// view of the just-unlocked root volume as the `/System/Logs` +
/// `/System/Settings` backing (`PREREQUISITES.md` P-B).
///
/// The unlock itself cannot do this: it holds the key but not a second
/// `'static` block window onto the root partition (it reuses one borrowed
/// window per passphrase attempt). The live implementation captures the
/// `'static` driver-store device, opens the read-write [`ARXFS`] window
/// onto the `ARXFSRoot` partition under the supplied key, and registers it
/// through `crate::system_mount::register_writable_state`. It is
/// **fail-soft**: publishing the writable backing is never allowed to abort
/// an otherwise-successful unlock.
pub trait WritableRootSink {
    /// Open a writable view of the just-mounted root volume under `keying`
    /// and publish it as the writable-state backing.
    ///
    /// Additionally returns the **registered driver itself** — the one
    /// `SleepLock`-serialised instance the `fs_*` syscalls run against —
    /// so the `CAP_USER_ADMIN` account-administration engine's storage
    /// (`plans/CAPABILITY_USE.md` CU4) shares it: the volume has exactly
    /// one writer, never a second independent window whose copy-on-write
    /// allocation state could diverge and corrupt the device.
    /// `/System/Security` is shadowed by the read-only `/System` mount, so
    /// the engine persists through this driver directly, below the VFS.
    /// `None` leaves account administration failing closed
    /// (`NotImplemented`).
    fn publish(&self, keying: Keying<'_>) -> Option<Arc<SleepLock<Box<dyn KernelFs>>>>;
}

/// A [`WritableRootSink`] that publishes nothing.
///
/// Used by the two-device load path and the host tests, which exercise the
/// users/identity load without a live driver-store device to open a second
/// writable window from. The writable `/System/Logs` backing then simply
/// stays unregistered and its operations fail closed `NotImplemented`.
pub struct NoWritableRootSink;

impl WritableRootSink for NoWritableRootSink {
    fn publish(&self, _keying: Keying<'_>) -> Option<Arc<SleepLock<Box<dyn KernelFs>>>> {
        None
    }
}

/// The set-once destinations a successful unlock publishes into.
///
/// Bundled so the unlock policy carries one "install targets" reference
/// rather than a row of parallel parameters: the validated users database,
/// the verified identity table, the writable-state sink, and the
/// storage-group gid cell. All are published together on a successful
/// unlock.
pub struct UnlockInstall<'a> {
    /// The set-once cell the validated `users-v1` text is published into.
    pub users: &'a LateUsersDb,
    /// The set-once cell the verified user/group identity table is published
    /// into (the `fs_*` group-resolution source).
    pub identity: &'a LateIdentity,
    /// The cell the validated `groups-v1` text is published into, so the
    /// ungated group directory can render a gid as a name.
    pub groups: &'a LateGroupsDb,
    /// The sink that publishes the writable `/System/Logs` + `/System/Settings`
    /// backing from a second read-write view of the just-unlocked volume.
    pub writable: &'a dyn WritableRootSink,
    /// The `CAP_USER_ADMIN` engine install targets, when the boot path
    /// wires account administration (`plans/CAPABILITY_USE.md` CU4).
    /// `None` (host tests, ports without a writable root) leaves the
    /// `users_admin` syscall failing closed.
    pub admin: Option<AdminInstall>,
    /// The set-once cell the well-known `storage` group's gid is resolved
    /// into from the loaded group registry (`plans/DEVICES.md` D3d — the
    /// removable-volume identity map's policy input). A registry without
    /// that group installs nothing, leaving foreign volumes system-owned
    /// (fail closed, never an invented gid).
    pub storage_gid: &'a LateStorageGid,
}

/// The `'static` targets the account-administration engine is built over
/// on a successful unlock (`plans/CAPABILITY_USE.md` CU4).
///
/// Carried separately from the [`UnlockInstall`] cell borrows because the
/// engine is stored for the kernel's lifetime and therefore needs the
/// `'static` form of the same cells; production boot paths hand the same
/// statics to both.
pub struct AdminInstall {
    /// The set-once cell the built engine is published into (the
    /// `users_admin` dispatch target).
    pub cell: &'static LateUsersAdmin,
    /// The live users-database cell the engine swaps on each commit — the
    /// same cell as [`UnlockInstall::users`], in `'static` form.
    pub users: &'static LateUsersDb,
    /// The live identity-table cell the engine swaps on each commit — the
    /// same cell as [`UnlockInstall::identity`], in `'static` form.
    pub identity: &'static LateIdentity,
    /// The live group-registry cell the engine swaps on each commit — the
    /// same cell as [`UnlockInstall::groups`], in `'static` form.
    pub groups: &'static LateGroupsDb,
    /// The audit sink the engine records every operation outcome to.
    pub audit: &'static (dyn Sink + Sync),
}

impl core::fmt::Debug for UnlockedRoot {
    /// Redacted: the held users text is salted credential records and must
    /// never reach a log or panic message (the `HeldUsersDbSource` Debug is
    /// itself redacted). Only the presence of each half is printed.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UnlockedRoot")
            .field("users", &self.users)
            .field("identity", &"<installed>")
            .finish_non_exhaustive()
    }
}

/// Mount the root volume `block` under `keying` and load its
/// `/System/Security/Users` database and `/System/Security/Groups` registry,
/// building the kernel's identity table from the two.
///
/// On success the returned [`UnlockedRoot`] owns the validated `users-v1`
/// text (zeroed on drop), the verified identity table, and `keying`, so the
/// writable-state sink can open the same volume read-write.
///
/// # Errors
///
/// A [`RootMountError`] naming the first check that refused. Every error path
/// yields no database and is audited; an encrypted root's key is wiped
/// regardless of outcome.
fn load_root<B: Block>(
    block: B,
    keying: RootKeying,
    image: ImageConfig,
    audit: &dyn Sink,
) -> Result<UnlockedRoot, RootMountError> {
    let mut fs = match ARXFS::open(block, keying.keying()) {
        Ok(fs) => fs,
        Err(err) => {
            let error = RootMountError::Mount(err);
            reject(audit, error);
            return Err(error);
        }
    };
    let protection = match keying {
        RootKeying::Plaintext => "plaintext",
        RootKeying::Key(_) => "encrypted",
    };
    log(
        audit,
        &Event {
            level: Level::Info,
            id: ROOT_MOUNT_UNLOCKED,
            message: "root-mount: root volume mounted",
            fields: &[Field {
                key: "protection",
                value: tairix_log::FieldValue::Str(protection),
            }],
        },
    );

    let users = load_users_db_source(&mut fs, audit).map_err(RootMountError::Users)?;
    // The group registry lives on the same root, so the identity table is
    // built here while the volume is open; a refusal installs no partial
    // identity.
    let groups_db = load_groups_db(&mut fs, audit).map_err(RootMountError::Groups)?;
    // Fail-safe: an absent or malformed store leaves every cache enabled and
    // never fails the mount — the caches are accelerators, never the truth.
    tairix_kernel_core::load_and_apply_system_config(
        &mut fs,
        image,
        &tairix_kernel_core::CACHE_CONTROL,
        audit,
    );
    let users_snapshot = users
        .text()
        .map_err(|_| RootMountError::Identity(Errno::NotImplemented))?;
    let users_text = core::str::from_utf8(&users_snapshot)
        .map_err(|_| RootMountError::Identity(Errno::BadMagic))?;
    let users_db =
        UsersDb::parse(users_text).map_err(|_| RootMountError::Identity(Errno::BadMagic))?;
    let identity =
        build_identity_table(&users_db, &groups_db, audit).map_err(RootMountError::Identity)?;

    Ok(UnlockedRoot {
        users,
        identity,
        users_db,
        groups_db,
        keying,
    })
}

/// The disk's partition table, parsed fail-closed (MBR or GPT) and audited
/// when it is refused.
fn partition_table<Disk: Block>(
    disk: &mut Disk,
    audit: &dyn Sink,
) -> Result<tairix_partition::PartitionTable, RootMountError> {
    parse_partition_table(disk).map_err(|err| {
        let error = RootMountError::PartitionTable(err);
        reject(audit, error);
        error
    })
}

/// A bounds-checked window onto `extent` of `disk`, audited when it does not
/// fit the device.
fn partition_window<'d, Disk: Block>(
    disk: &'d mut Disk,
    extent: &tairix_partition::Partition,
    audit: &dyn Sink,
) -> Result<PartitionBlock<&'d mut Disk>, RootMountError> {
    PartitionBlock::from_partition(disk, extent).map_err(|err| {
        let error = RootMountError::PartitionWindow(err);
        reject(audit, error);
        error
    })
}

/// Audit `error` and return it, for a refusal found outside a helper that
/// audits its own.
fn refused<T>(audit: &dyn Sink, error: RootMountError) -> Result<T, RootMountError> {
    reject(audit, error);
    Err(error)
}

/// Read how `disk`'s root volume is locked, auditing nothing: its own
/// protection word, checked against the boot partition's `root.unlock`
/// descriptor. The two must agree — a descriptor beside a plaintext root, or an
/// encrypted root with none, is a disk that is not what it claims, and is
/// refused before anything mounts.
///
/// The partitions are located by role, not index, and opened in sequence over
/// the one borrowed `disk`. A plaintext root needs no boot partition.
///
/// # Errors
///
/// [`RootMountError::PartitionTable`], [`RootMountError::NoRootPartition`],
/// [`RootMountError::PartitionWindow`], [`RootMountError::Mount`] for a root
/// that does not probe, [`RootMountError::DescriptorBesidePlaintextRoot`], or —
/// for an encrypted root — [`RootMountError::NoBootPartition`],
/// [`RootMountError::DescriptorRead`] and [`RootMountError::Descriptor`].
pub fn probe_root_lock<Disk: Block>(disk: &mut Disk) -> Result<RootLock, RootMountError> {
    let table = parse_partition_table(&mut *disk).map_err(RootMountError::PartitionTable)?;
    let root_extent = table
        .first_of_type(PartitionType::ARXFSRoot)
        .ok_or(RootMountError::NoRootPartition)?;
    let protection = {
        let mut root = PartitionBlock::from_partition(&mut *disk, &root_extent)
            .map_err(RootMountError::PartitionWindow)?;
        ARXFS::probe_protection(&mut root).map_err(RootMountError::Mount)?
    };
    let boot_extent = table.first_of_type(PartitionType::FatBoot);
    let descriptor = match &boot_extent {
        Some(boot_extent) => {
            let boot = PartitionBlock::from_partition(&mut *disk, boot_extent)
                .map_err(RootMountError::PartitionWindow)?;
            match read_root_unlock_descriptor(boot) {
                Ok(bytes) => Some(bytes),
                Err(DriverError::NotFound) => None,
                Err(err) => return Err(RootMountError::DescriptorRead(err)),
            }
        }
        None => None,
    };
    match (protection, descriptor) {
        (VolumeProtection::Plaintext, None) => Ok(RootLock::Plaintext),
        (VolumeProtection::Plaintext, Some(_)) => {
            Err(RootMountError::DescriptorBesidePlaintextRoot)
        }
        (VolumeProtection::Encrypted, Some(bytes)) => UnlockDescriptor::decode(&bytes)
            .map(RootLock::Passphrase)
            .map_err(RootMountError::Descriptor),
        (VolumeProtection::Encrypted, None) => Err(match boot_extent {
            None => RootMountError::NoBootPartition,
            Some(_) => RootMountError::DescriptorRead(DriverError::NotFound),
        }),
    }
}

/// [`probe_root_lock`] as a mount decides on it: a refusal is audited.
///
/// # Errors
///
/// As [`probe_root_lock`].
pub fn read_root_lock<Disk: Block>(
    disk: &mut Disk,
    audit: &dyn Sink,
) -> Result<RootLock, RootMountError> {
    probe_root_lock(disk).inspect_err(|&error| reject(audit, error))
}

/// Mount `disk`'s root volume as `unlock` says and load its users database,
/// group registry and identity table (`plans/PI.md` P11 Chunk B-2).
///
/// The entry the boot path and the Supervisor use, over the one whole-disk
/// [`Block`] device a board brings up — an SD card, a USB stick, a `virt`
/// virtio-blk image — MBR or GPT alike, with no board `cfg`. Driver loading is
/// not part of it: that is the design-B `/System`-volume path `devmgr` drives
/// over the driver-store endpoint ([`with_system_volume`] /
/// [`crate::driver_store_server`]).
///
/// # Errors
///
/// A [`RootMountError`] naming the first check that refused: the partition
/// table, the root partition or its window, or the mount and the load of its
/// databases — including a root whose protection is not the one `unlock`
/// expects. Every error path yields no database and is audited.
pub fn mount_root_disk_and_load_users<Disk: Block>(
    mut disk: Disk,
    unlock: RootUnlock<'_>,
    audit: &dyn Sink,
) -> Result<UnlockedRoot, RootMountError> {
    let table = partition_table(&mut disk, audit)?;
    let Some(root_extent) = table.first_of_type(PartitionType::ARXFSRoot) else {
        return refused(audit, RootMountError::NoRootPartition);
    };
    let keying = match unlock {
        RootUnlock::Plaintext => RootKeying::Plaintext,
        RootUnlock::Passphrase {
            descriptor,
            passphrase,
        } => match descriptor.derive_volume_key(passphrase) {
            Ok(key) => RootKeying::Key(key),
            // No root is provisioned under an empty or over-long passphrase,
            // so one is simply wrong.
            Err(_) => {
                return refused(audit, RootMountError::Mount(DriverError::PermissionDenied));
            }
        },
    };
    let image = image_system_config(&mut disk, &table);
    let root = partition_window(&mut disk, &root_extent, audit)?;
    load_root(root, keying, image, audit)
}

/// The image's default of the machine configuration, off `disk`'s read-only
/// `/System` volume, read before the root is: it stands only where the root
/// carries no administrator's layer. A disk with no `/System` volume carries
/// no default.
fn image_system_config<Disk: Block>(disk: &mut Disk, table: &PartitionTable) -> ImageConfig {
    let Some(extent) = table.first_of_type(PartitionType::ARXFSSystem) else {
        return Ok(None);
    };
    let unreadable = |_| BoundedReadError::Vfs(VfsError::Io);
    let window = PartitionBlock::from_partition(disk, &extent).map_err(unreadable)?;
    let mut fs = ARXFS::open_read_only(window, Keying::Plaintext).map_err(unreadable)?;
    read_image_system_config(&mut fs)
}

/// Read what mounting `disk`'s root will read, so the mount after the
/// Supervisor window finds it cached: a plaintext root through a read-only
/// open and the bootstrap files the unlock loads, an encrypted root — which
/// nothing can read without its key — as far as its lock.
///
/// Writes nothing and records nothing; whatever it cannot read, the mount
/// reads and reports.
pub fn prefetch_root<Disk: Block>(mut disk: Disk) {
    let Ok(RootLock::Plaintext) = probe_root_lock(&mut disk) else {
        return;
    };
    let Ok(table) = parse_partition_table(&mut disk) else {
        return;
    };
    let Some(extent) = table.first_of_type(PartitionType::ARXFSRoot) else {
        return;
    };
    let Ok(root) = PartitionBlock::from_partition(&mut disk, &extent) else {
        return;
    };
    let Ok(mut fs) = ARXFS::open_read_only(root, Keying::Plaintext) else {
        return;
    };
    for file in [USERS_DB, GROUPS_DB, SYSTEM_CONFIG] {
        if let Ok(mut bytes) = read_bootstrap_file(&mut fs, file) {
            // The users database holds password hashes.
            bytes.zeroize();
        }
    }
}

/// Discover and mount the read-only signed-bundle `/System` volume on
/// `disk` and run the continuation `f` against the **still-open** volume,
/// returning whatever `f` returns wrapped in [`Some`].
///
/// This is the one place the `/System` discovery + read-only mount + layout
/// confirmation lives: the Design D persistent
/// driver-store service
/// ([`crate::driver_store_server::serve_system_store`]) runs its
/// never-returning serve loop through it, building a
/// [`SystemFileService`](crate::system_files::SystemFileService) over the
/// mounted volume to serve the `devmgr` catalogue/load requests. Because the
/// mounted volume borrows
/// the [`PartitionBlock`] window which borrows `disk`, all of it lives on
/// the caller's frame for as long as `f` runs — so a continuation that never
/// returns (the persistent server) keeps the mount live for the life of the
/// system without any `'static` promotion.
///
/// The disk's partition table is parsed fail-closed; if it carries a
/// [`PartitionType::ARXFSSystem`] partition, a bounds-checked
/// [`PartitionBlock`] window is opened over it and mounted **read-only** as
/// the plaintext volume it is ([`ARXFS::open_read_only`] — it holds no secrets
/// and its integrity rests on the per-bundle Ed25519 signatures the load gate
/// verifies). The volume's root is probed for the
/// `Drivers` store directory to confirm it is a real `/System` volume, the
/// mount is audited (`SYSTEM_VOLUME_MOUNTED`), and only then is `f` run.
///
/// **Fail-soft and fail-closed**: a disk with no
/// — or an unopenable — `/System` volume returns [`None`] (each decline
/// audited `SYSTEM_VOLUME_UNAVAILABLE`) without running `f`, and never
/// aborts the boot.
///
/// * `disk` — the whole-disk [`Block`] device the board brought up.
/// * `audit` — the sink every decision is logged through. No secret is consumed or logged.
/// * `cache` — the volume's transform cache, shared with its other mounts, for
///   a window that reads bundles; `None` for one that only lists.
/// * `f` — the continuation run against the mounted read-only `/System`
///   volume.
pub fn with_system_volume<Disk: Block, R>(
    disk: &mut Disk,
    audit: &dyn Sink,
    cache: Option<SharedClusterCache>,
    f: impl FnOnce(&mut dyn RootVolume) -> R,
) -> Option<R> {
    let Ok(table) = parse_partition_table(&mut *disk) else {
        system_volume_unavailable(audit, "partition_table_invalid");
        return None;
    };
    let Some(extent) = table.first_of_type(PartitionType::ARXFSSystem) else {
        system_volume_unavailable(audit, "no_system_partition");
        return None;
    };
    // A bounds-checked window onto the `/System` extent.
    let Ok(window) = PartitionBlock::from_partition(&mut *disk, &extent) else {
        system_volume_unavailable(audit, "system_window_out_of_range");
        return None;
    };
    let Ok(mut system) = ARXFS::open_read_only(window, Keying::Plaintext) else {
        system_volume_unavailable(audit, "system_mount_failed");
        return None;
    };
    if let Some(cache) = cache {
        system = system.with_cluster_cache(alloc::boxed::Box::new(cache));
    }
    // Confirm it is a real `/System` volume: its root carries the
    // `Drivers` store directory the continuation reads.
    let root = system.root();
    if system.lookup(root, b"Drivers").is_err() {
        system_volume_unavailable(audit, "system_layout_invalid");
        return None;
    }
    log(
        audit,
        &Event {
            level: Level::Info,
            id: SYSTEM_VOLUME_MOUNTED,
            message: SYSTEM_VOLUME_MOUNTED_MESSAGE,
            fields: &[],
        },
    );
    Some(f(&mut system))
}

/// Audit a declined `/System` mount with a stable, secret-free `cause`.
/// Fail-soft, so this is always informational and the boot proceeds with no
/// driver autoloaded.
fn system_volume_unavailable(audit: &dyn Sink, cause: &'static str) {
    log(
        audit,
        &Event {
            level: Level::Info,
            id: SYSTEM_VOLUME_UNAVAILABLE,
            message: "root-mount: no read-only /System volume mounted",
            fields: &[Field {
                key: "cause",
                value: tairix_log::FieldValue::Str(cause),
            }],
        },
    );
}

/// Audit event: the operator's typed passphrase unlocked the root and the
/// loaded database was published into the late credential cell — login can
/// now authenticate. No secret is logged.
const ROOT_UNLOCK_INSTALLED: EventId = EventId(4136);

/// Audit event: a wrong passphrase was refused; after the anti-brute-force
/// delay the operator is told and prompted again (the unlock re-prompts a
/// wrong passphrase indefinitely, since the root cannot mount without the
/// right one). No passphrase byte is logged.
const ROOT_UNLOCK_RETRY: EventId = EventId(4137);

/// Audit event: the interactive unlock gave up fail-closed — the console
/// could not be read, the disk's structure was wrong, or the late cell was
/// already populated. A wrong passphrase never lands here: it is re-prompted,
/// not given up on. No database is installed and the operator must reboot.
/// The `cause` field names which check refused, secret-free.
const ROOT_UNLOCK_GAVE_UP: EventId = EventId(4138);

/// The single source of the encrypted-root filesystem's operator-facing
/// label. Both the unlock prompt and the post-unlock line are built from
/// this one token, so renaming the filesystem cannot leave the two
/// spellings out of step.
macro_rules! fs_label {
    () => {
        "ARXFS"
    };
}

/// The fresh line the Supervisor's announcement opens.
macro_rules! window_line {
    () => {
        "\r\n"
    };
}

/// The Supervisor's announcement, wherever on its line it is drawn.
macro_rules! announcement {
    () => {
        "[Press ESC for supervisor]"
    };
}

/// The interactive unlock prompt drawn on its own fresh line. `\r\n` opens
/// the line; the trailing space separates the label from the typed secret.
const FS_UNLOCK_PROMPT: &[u8] = concat!("\r\n", fs_label!(), " passphrase: ").as_bytes();

/// The line a mounted root leaves behind, followed by one blank line.
///
/// A carriage return returns the cursor to the start of the prompt or
/// announcement line, the bare label overwrites it, and the
/// erase-to-end-of-line control clears whatever the operator typed (and the
/// `[input complete]` feedback marker) off the tail — so the line reads just
/// the label. The `\r\n\r\n` then ends that line and leaves one blank line
/// separating the unlock from the login `Username:` prompt that follows.
const FS_UNLOCKED_LINE: &[u8] = concat!("\r", fs_label!(), "\x1b[K\r\n\r\n").as_bytes();

/// What the same line says when the boot gives the root up, before the cause.
const FS_GAVE_UP_PREFIX: &[u8] = concat!("\r", fs_label!(), ": root not mounted: ").as_bytes();

/// The interactive unlock prompt drawn **in place** over the boot-screen
/// announcement line, used for the first prompt after the ESC window has
/// already drawn on the line (`plans/NEW-SUPERVISOR.md` §2 step 3). The `\r`
/// returns to column 0, the label overwrites `[Press ESC for supervisor]`,
/// and `\x1b[K` erases the longer announcement's tail — the same in-place
/// technique [`FS_UNLOCKED_LINE`] uses. Built from the one [`fs_label!`] token
/// so the two prompt spellings can never drift.
const FS_UNLOCK_PROMPT_INPLACE: &[u8] =
    concat!("\r", fs_label!(), " passphrase: \x1b[K").as_bytes();

/// The byte-exact boot-screen announcement the ESC window draws
/// (`plans/NEW-SUPERVISOR.md` §2 step 1). The leading `\r\n` opens a fresh
/// line, so one blank line separates the boot banner (the machine-summary
/// line userland `init` printed beneath the `TAIRiX <v> <RAM>MiB` counter)
/// from the announcement — the same one-blank-line spacing the passphrase
/// prompt draws on its own fresh line ([`FS_UNLOCK_PROMPT`]), so the operator
/// sees an identically laid-out screen whether the window is entered, skipped,
/// or times out. The in-place prompt and the enter-banner below return to
/// column 0 of *this* announcement line and overwrite its text. Frozen
/// wording — asserted by the QEMU vertical; do not "improve" it.
const SUPERVISOR_ANNOUNCE: &[u8] = concat!(window_line!(), announcement!()).as_bytes();

/// The window's line while console 0 cannot yet deliver a key, so the wait
/// shows rather than looking like a boot that skipped the window.
const KEY_SOURCE_AWAITED: &[u8] = concat!(window_line!(), "[Waiting for a keyboard]").as_bytes();

/// The announcement drawn over [`KEY_SOURCE_AWAITED`] once a key can arrive,
/// with whatever of the waiting text it does not cover erased.
const ANNOUNCE_OVER_WAIT: &[u8] = concat!("\r", announcement!(), "\x1b[K").as_bytes();

/// [`KEY_SOURCE_AWAITED`] erased when no key can arrive, leaving its line open
/// for the unlock as an elapsed window leaves it.
const KEY_SOURCE_ABANDONED: &[u8] = b"\r\x1b[K";

/// How long the window waits for console 0 to gain a key source before the
/// boot goes on without it.
pub const KEY_SOURCE_WAIT_NS: u64 = 30_000_000_000;

/// Whether console 0 can deliver a key, as the Supervisor window asks it.
pub trait KeyArrival {
    /// Whether a key can arrive now.
    fn key_source_ready(&self) -> bool;

    /// Return once a key can arrive, or [`KEY_SOURCE_WAIT_NS`] after the call
    /// with none: whether one can.
    fn await_key_source(&self) -> bool;
}

/// The pre-boot Supervisor a boot can drop into, and the key source its
/// window waits for.
pub struct SupervisorWindow<'a> {
    /// The REPL's host.
    pub host: &'a mut dyn SupervisorHost,
    /// When console 0 can deliver the key that opens it.
    pub keys: &'a dyn KeyArrival,
}

/// The byte-exact line collapse + banner drawn when the operator drops into
/// the Supervisor (`plans/NEW-SUPERVISOR.md` §2 step 5): the prompt/announce
/// line is overwritten with the bare label, ended, one blank line follows,
/// then the `Supervisor` banner. `run_supervisor` draws its own `* ` prompt
/// after this. Frozen wording — asserted by the QEMU vertical.
const SUPERVISOR_ENTER_BANNER: &[u8] =
    concat!("\r", fs_label!(), "\x1b[K\r\n\r\nSupervisor\r\n").as_bytes();

/// The 2-second boot-screen window during which a lone `ESC` drops to the
/// Supervisor (`plans/NEW-SUPERVISOR.md` §2 step 2), in nanoseconds. A
/// deliberate fixed operator-facing delay, not a scalable capacity.
const SUPERVISOR_WINDOW_NS: u64 = 2_000_000_000;

/// The short bounded re-poll, in nanoseconds, that disambiguates a lone `ESC`
/// (drop to Supervisor) from an `ESC [ …` CSI sequence (an editor key)
/// (`plans/NEW-SUPERVISOR.md` §3). Tens of milliseconds is long enough that a
/// terminal's CSI bytes, sent back-to-back after the `ESC`, arrive within it,
/// yet short enough not to make a genuine lone `ESC` feel sluggish.
const SUPERVISOR_ESC_REPOLL_NS: u64 = 50_000_000;

/// The one set-once credential cell the production dispatch hook reads
/// and the in-kernel root-unlock kthread writes.
///
/// The root is mounted only *after* the console keyboard is live — past the
/// point where [`BootInfo::with_users_db`] is consumed
/// (`plans/PI.md` P11 Chunk B-2). The boot path therefore hands the
/// dispatch hook `&LATE_USERS_DB` at boot through
/// [`BootInfo::with_users_db`], so `users_db_read` reads
/// [`UsersDbSource::text`] on the same cell on every call; the trusted
/// unlock step publishes the mounted root volume's database into it
/// exactly once via [`LateUsersDb::install`], and the next read serves it.
///
/// Defined here, beside the unlock policy that installs into it, so the
/// dispatch-hook reference and the kthread's install target are one
/// definition rather than two that could diverge. It is
/// **not** a global mutable static: the cell is set-once
/// and immutable after the first install, with internal synchronisation,
/// so the single `&'static` instance is shared safely across the per-CPU
/// syscall handlers exactly like
/// [`NULL_USERS_DB`](tairix_kernel_core::NULL_USERS_DB). Until an install
/// succeeds it fails every read closed, so wiring the dispatch hook at it
/// changes no boot behaviour over the previous `NULL_USERS_DB` default.
///
/// [`BootInfo::with_users_db`]: tairix_kernel_core::BootInfo::with_users_db
/// [`UsersDbSource::text`]: tairix_kernel_core::UsersDbSource::text
pub static LATE_USERS_DB: LateUsersDb = LateUsersDb::new();

/// The set-once `CAP_USER_ADMIN` engine cell the `users_admin` syscall
/// dispatches into (`plans/CAPABILITY_USE.md` CU4), installed by the same
/// trusted unlock step that installs [`LATE_USERS_DB`] and
/// [`LATE_IDENTITY`]. Until then every `users_admin` call fails closed
/// with `NotImplemented`.
pub static LATE_USERS_ADMIN: LateUsersAdmin = LateUsersAdmin::new();

/// The authoritative user/group identity cell the `fs_*` syscalls resolve
/// a caller's groups against (`PREREQUISITES.md` P-A). The kernel core's
/// sec phase installs the compiled-in system identity into it at boot
/// (every port hands it over through `with_spawn_identity`); the trusted
/// unlock step that installs [`LATE_USERS_DB`] then replaces the held
/// table with the merged system∪human table.
///
/// Held by the production [`MountedFilesystemService`](tairix_kernel_core::MountedFilesystemService)
/// the dispatch hook serves the `fs_*` syscalls through. From the sec
/// phase onward the system accounts resolve; a *human* uid resolves only
/// once the unlock published the merged table (fail closed before then) —
/// the credential-free table carries the uid → group/capability mapping
/// the VFS needs, never password material, so the long-lived `&'static`
/// copy leaks no secret. Defined beside [`LATE_USERS_DB`] so the two
/// halves the one unlock publishes are one definition.
pub static LATE_IDENTITY: LateIdentity = LateIdentity::new();

/// The live group registry the ungated group directory is served from.
///
/// Published by the trusted unlock step beside [`LATE_USERS_DB`] and
/// replaced by the audited admin engine's commit. Carries no credential,
/// so it needs neither the users cell's set-once install nor its pending
/// state: before the unlock the directory answers the compiled-in system
/// groups alone.
pub static LATE_GROUPS_DB: LateGroupsDb = LateGroupsDb::new();

/// The result of [`unlock_root_disk_interactively`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum UnlockOutcome {
    /// The root unlocked under a typed passphrase and the loaded database
    /// was installed into the late credential cell. Login can authenticate.
    Installed,
    /// The unlock gave up fail-closed: no database was installed, so every
    /// login is refused until the next boot. The
    /// caller (the unlock kthread) must not retry — the operator reboots.
    GaveUp,
}

/// Why a single passphrase line could not be read off the console.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum PassphraseReadError {
    /// The console input device failed or could not deliver a line (a
    /// device error, or no input source at all). The unlock fails closed
    /// rather than treating it as an empty passphrase.
    Console,
    /// The line exceeded [`MAX_ROOT_PASSPHRASE_LEN`] before a terminator; the
    /// remainder was drained to the newline so the next read starts clean.
    /// Treated as a wrong attempt, never a truncated secret.
    TooLong,
    /// A lone `ESC` was pressed as the **first** byte of the line: not a
    /// read failure but a request to drop into the Supervisor from the live
    /// passphrase prompt (`plans/NEW-SUPERVISOR.md` §2 step 4). No passphrase
    /// byte was collected.
    Escape,
}

/// Publish a successfully mounted root's database into `late_db` and audit
/// it.
///
/// Shared by every mount path — a plaintext root, each interactive attempt,
/// and the Supervisor's `mount` — so the set-once install, its fail-closed
/// "already installed" guard, and the [`ROOT_UNLOCK_INSTALLED`] audit line
/// have exactly one definition. The install
/// ([`LateUsersDb::install`]) is set-once: a refusal means a database was
/// already published (the kthread runs once, so that is a logic error),
/// and the rejected holder is zeroed inside `install`.
pub(crate) fn finish_install(
    unlocked: UnlockedRoot,
    install: &UnlockInstall<'_>,
    audit: &dyn Sink,
) -> UnlockOutcome {
    let UnlockedRoot {
        users,
        identity,
        users_db,
        groups_db,
        keying,
    } = unlocked;
    // Publish the writable `/System/Logs` + `/System/Settings` backing while
    // an encrypted root's key is still live — and receive the registered
    // driver itself, which the account-administration engine shares (one
    // volume, one writer). Fail-soft: a publish refusal leaves those subtrees
    // (and account administration) failing closed and never blocks the
    // users/identity install below. The key is wiped when `keying` drops.
    let admin_fs = install.writable.publish(keying.keying());
    // Publish the merged identity table first, so the `fs_*` group
    // resolution covers the human accounts before `login` can act on the
    // installed users database. The boot sec phase installed the
    // compiled-in system identity into this same cell; the unlock
    // *replaces* it with the merged system∪human table built from the
    // loaded databases. A refusal means the boot never installed a table
    // — a boot-path logic error — and the unlock fails closed rather than
    // serve half an identity (the rejected table carries no secret; the
    // users holder is zeroed inside `install`).
    if install.identity.replace(identity).is_err() {
        gave_up(audit, "identity_replace_refused");
        return UnlockOutcome::GaveUp;
    }
    if install.users.install(users).is_err() {
        gave_up(audit, "already_installed");
        return UnlockOutcome::GaveUp;
    }
    // Resolve the well-known removable-storage group **by name** from the
    // just-loaded registry and arm the ownerless-volume identity map
    // (`plans/DEVICES.md` D3d). A registry without the group installs
    // nothing: foreign volumes then stay system-owned rather than mounted
    // under an invented gid.
    if let Some(group) = groups_db.lookup(tairix_users::STORAGE_GROUP) {
        install.storage_gid.install(GroupId(group.gid().0));
    }
    // The registry carries no credential, so it is published as its own
    // text for the ungated group directory to render names from.
    install.groups.publish(groups_db.serialise().into_bytes());
    // With both live cells installed, build and publish the
    // `CAP_USER_ADMIN` engine over the same verified state
    // (`plans/CAPABILITY_USE.md` CU4). Fail-soft: a boot path that wires
    // no admin targets, or a volume that yielded no admin window, leaves
    // `users_admin` failing closed — never a partial engine.
    if let (Some(admin), Some(fs)) = (&install.admin, admin_fs) {
        let backing: &'static RootAdminBacking<Box<dyn KernelFs>> =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(RootAdminBacking::new(fs)));
        let engine = UserAdminEngine::new(
            users_db,
            groups_db,
            admin.users,
            admin.identity,
            admin.groups,
            backing,
            admin.audit,
        );
        if admin.cell.install(engine).is_err() {
            gave_up(audit, "users_admin_already_installed");
            return UnlockOutcome::GaveUp;
        }
    }
    log(
        audit,
        &Event {
            level: Level::Info,
            id: ROOT_UNLOCK_INSTALLED,
            message: "root-unlock: root mounted; users database installed",
            fields: &[],
        },
    );
    UnlockOutcome::Installed
}

/// A `lib/supervisor` [`Report`](SupReport) backed by the primary console's
/// write half, so the Supervisor renders straight to the boot console.
struct ConsoleReport<'a> {
    console: &'a dyn ConsoleWrite,
}

impl SupReport for ConsoleReport<'_> {
    fn write_bytes(&mut self, bytes: &[u8]) {
        write_all(self.console, bytes);
    }
}

/// A `lib/supervisor` [`SupInput`] backed by the primary console's raw read
/// half. [`read_byte`](SupInput::read_byte) parks until a key arrives (never a
/// busy-spin — the bootstrap-floor console reader wakes on the RX interrupt's
/// `console_wake` and re-polls on a bounded timed backstop, see
/// [`KthreadConsoleRead`](crate::unlock_service::KthreadConsoleRead));
/// [`poll_byte`](SupInput::poll_byte) is a non-blocking zero-deadline read the
/// REPL uses to poll for an `ESC` abort during a long command.
struct ConsoleInput<'a> {
    input: &'a dyn ConsoleRead,
}

impl SupInput for ConsoleInput<'_> {
    fn read_byte(&mut self) -> Option<u8> {
        let mut byte = [0u8; 1];
        match self.input.read(&mut byte) {
            Ok(1) => Some(byte[0]),
            _ => None,
        }
    }

    fn poll_byte(&mut self) -> Option<u8> {
        let mut byte = [0u8; 1];
        match self.input.read_timeout(&mut byte, 0) {
            Ok(1) => Some(byte[0]),
            _ => None,
        }
    }
}

/// The outcome of the pre-unlock ESC boot-screen window.
enum EscWindow {
    /// The operator mounted the root from inside the Supervisor: the unlock is
    /// already resolved (the users database is installed), so the caller
    /// returns [`UnlockOutcome::Installed`] without prompting.
    Mounted,
    /// The operator resumed the boot from the Supervisor, which redrew the
    /// screen: the unlock draws its prompt on a fresh line.
    Resumed,
    /// The window elapsed, or the operator began typing: the announcement is
    /// still on its line, for the unlock to draw over. `initial` carries a
    /// byte the window already consumed when the operator began typing the
    /// passphrase before the window elapsed: it is the first character of the
    /// passphrase line and MUST be fed to the read rather than dropped, or the
    /// typed secret would be silently corrupted.
    Elapsed { initial: Option<u8> },
}

/// Resolve a just-read `ESC` byte into a lone `ESC` (drop to the Supervisor)
/// or the start of a CSI editor sequence (`ESC [ …` — an arrow / Delete key),
/// the classic terminal disambiguation (`plans/NEW-SUPERVISOR.md` §3).
///
/// Re-polls once, briefly and bounded, for a follow-on byte:
/// * a follow-on `[` opens a CSI sequence — the rest of it is drained so its
///   bytes cannot leak into the passphrase, and this reports **not** a lone
///   `ESC`;
/// * no follow-on within the window, or any non-`[` byte, is a lone `ESC`.
///
/// The re-poll is a genuine bounded timed park (never a busy-spin); a backing
/// that cannot honour the deadline degrades to treating `ESC` as lone.
fn esc_is_lone_escape(input: &dyn ConsoleRead) -> bool {
    let mut byte = [0u8; 1];
    match input.read_timeout(&mut byte, SUPERVISOR_ESC_REPOLL_NS) {
        Ok(1) if byte[0] == b'[' => {
            drain_csi_sequence(input);
            false
        }
        _ => true,
    }
}

/// Drain the remainder of a CSI sequence whose `ESC [` prefix was already
/// consumed, up to and including its final byte (`0x40..=0x7e`), so no
/// sequence byte leaks into a later read. Bounded by a short per-byte re-poll
/// and stops on the final byte, a read failure, or an idle window — never
/// spins.
fn drain_csi_sequence(input: &dyn ConsoleRead) {
    for _ in 0..16 {
        let mut byte = [0u8; 1];
        match input.read_timeout(&mut byte, SUPERVISOR_ESC_REPOLL_NS) {
            Ok(1) => {
                if (0x40..=0x7e).contains(&byte[0]) {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// Draw the boot-screen announcement, wait a bounded window for `ESC`, and —
/// on a lone `ESC` — run the Supervisor REPL. Byte-exact per
/// `plans/NEW-SUPERVISOR.md` §2.
///
/// The timed read and the re-poll are genuine bounded parks on the
/// interrupt-driven console reader (never a busy-spin).
///
/// A lone `ESC` inside the window drops to the Supervisor. An `ESC [ …` CSI
/// editor sequence is drained and ignored (it is a stray arrow/Delete key, not
/// passphrase content). **Any other key is the operator starting to type the
/// passphrase before the window elapsed**: it is carried out as
/// [`EscWindow::Elapsed::initial`] and fed to the passphrase read as the
/// line's first character — never discarded, which would silently corrupt a
/// quickly-typed secret. A root that takes no passphrase sends it nowhere.
///
/// The countdown starts only once console 0 can deliver a key; until then the
/// line says the window waits for one. When none can arrive in
/// [`KeyArrival::await_key_source`]'s bound the waiting text is erased and its
/// line left for the unlock as an elapsed window leaves it.
fn esc_boot_window(
    console: &dyn ConsoleWrite,
    input: &dyn ConsoleRead,
    supervisor: &mut SupervisorWindow<'_>,
    boot_session: &LateBootSession,
) -> EscWindow {
    if supervisor.keys.key_source_ready() {
        write_all(console, SUPERVISOR_ANNOUNCE);
    } else {
        write_all(console, KEY_SOURCE_AWAITED);
        if !supervisor.keys.await_key_source() {
            write_all(console, KEY_SOURCE_ABANDONED);
            return EscWindow::Elapsed { initial: None };
        }
        write_all(console, ANNOUNCE_OVER_WAIT);
    }
    let mut byte = [0u8; 1];
    let got = input
        .read_timeout(&mut byte, SUPERVISOR_WINDOW_NS)
        .unwrap_or(0);
    if got == 1 && byte[0] == 0x1b {
        if esc_is_lone_escape(input) {
            // Lone ESC: enter the REPL.
            return match enter_supervisor(console, input, supervisor.host, boot_session) {
                // The operator unlocked and mounted the root from the REPL.
                SupervisorExit::Mounted => EscWindow::Mounted,
                // The operator asked to resume boot. Any session choice the
                // operator made was already recorded by `enter_supervisor`.
                SupervisorExit::ContinueBoot(_) => EscWindow::Resumed,
            };
        }
        // A CSI editor sequence (arrow/Delete): its bytes were drained by
        // `esc_is_lone_escape`, so nothing is carried forward.
        return EscWindow::Elapsed { initial: None };
    }
    // Either the window timed out (`got == 0`) or the operator typed a
    // non-`ESC` key — the first character of the passphrase, carried forward
    // so it is not lost.
    EscWindow::Elapsed {
        initial: (got == 1).then_some(byte[0]),
    }
}

/// Collapse the current prompt/announcement line to the bare label, draw the
/// `Supervisor` banner, and run the REPL to its exit (byte-exact per
/// `plans/NEW-SUPERVISOR.md` §2 step 5). Shared by both entry points — the
/// ESC boot-screen window and a lone `ESC` at the live passphrase prompt — so
/// the banner and the REPL invocation have one definition.
///
/// A `continue text` / `continue gui` choice is recorded in `boot_session`
/// here, at the one point both entry paths funnel through, so neither can
/// forget it. The cell is set-once and a bare `continue` chooses nothing, so
/// the first real choice of the boot is the one `boot_session_get` reports
/// (`plans/NEW-DESKTOP-LOGIN.md` G1).
fn enter_supervisor(
    console: &dyn ConsoleWrite,
    input: &dyn ConsoleRead,
    supervisor: &mut dyn SupervisorHost,
    boot_session: &LateBootSession,
) -> SupervisorExit {
    write_all(console, SUPERVISOR_ENTER_BANNER);
    let mut report = ConsoleReport { console };
    let mut sup_input = ConsoleInput { input };
    let exit = run_supervisor(&mut report, &mut sup_input, supervisor);
    if let SupervisorExit::ContinueBoot(session) = exit {
        boot_session.install(session);
    }
    exit
}

/// Mount the root disk and publish the loaded database into `late_db`
/// (`plans/PI.md` P11 Chunk B-2). A plaintext root ([`read_root_lock`]) mounts
/// with no prompt at all. An encrypted root is prompted for, re-prompting a
/// wrong typed passphrase **indefinitely**, each wrong attempt rate-limited by
/// `delay` and followed by an `Incorrect passphrase` notice, until the correct
/// one unlocks. An empty passphrase is simply a wrong one.
///
/// This is the device-independent unlock *policy* the in-kernel unlock
/// kthread runs once the board has brought up the root block device and
/// the console keyboard is live. It is generic over the [`Block`] disk and
/// takes the console write/read halves as object-safe seams
/// ([`ConsoleWrite`] / [`ConsoleRead`]), so it names no architecture or
/// device type and is host-tested with a mock console over the same MBR +
/// `ARXFS` disk fixtures `tools/mkimage` writes. The kthread passes the **blocking** console
/// read ([`BlockingConsoleRead`](tairix_kernel_core::BlockingConsoleRead))
/// so an empty poll parks the task on the scheduler rather than
/// busy-spinning; this function only moves bytes.
///
/// Each attempt on an encrypted root:
///
/// 1. Writes the `ARXFS passphrase:` prompt to `console` (best
///    effort — a write error does not by itself abort the attempt).
/// 2. Reads one line into a zeroized, fixed-length on-stack buffer
///    ([`MAX_ROOT_PASSPHRASE_LEN`]); the passphrase is never heap-allocated,
///    logged, or retained past the attempt.
/// 3. Runs [`mount_root_disk_and_load_users`] under the typed bytes.
///
/// On success the loaded [`HeldUsersDbSource`] is published into `late_db`
/// through [`LateUsersDb::install`] (set-once) and the function returns
/// [`UnlockOutcome::Installed`]. A wrong passphrase
/// ([`RootMountError::Mount`]`(`[`DriverError::PermissionDenied`]`)`) is
/// audited, rate-limited by `delay`, reported to the operator, and prompted
/// **again — indefinitely**: the encrypted root holds the only copy of the
/// user data and login is refused until it mounts, so there is nothing to
/// advance to without the correct passphrase and no bounded "give up" that
/// would help. Any other error is structural (no partition table, no
/// boot/root partition, an unreadable, invalid or contradicted descriptor, a
/// corrupt database): retrying cannot help, so the unlock gives up. A console
/// read error also gives up. Every give-up path leaves `late_db` empty — login
/// stays fail-closed.
///
/// * `disk` — the whole-disk [`Block`] device the board brought up.
/// * `console` — the primary console's byte sink for the prompt.
/// * `input` — the primary console's (blocking) byte source.
/// * `install` — the set-once publish destinations (users database, identity
///   table, and writable-state sink) a successful unlock fills together.
/// * `audit` — the sink every decision is logged through; no passphrase, key, or volume byte is ever logged.
/// * `delay` — the anti-brute-force pause run after each wrong passphrase,
///   before the operator is told and re-prompted. The production kthread
///   backs it with a minimum-duration timed park (never a busy-wait); host
///   tests pass a no-op.
/// * `on_resolved` — invoked exactly once, after the unlock reaches its
///   terminal outcome (installed or gave up) and on every internal return
///   path, so the caller can release the console it lent the prompt. The
///   in-kernel kthread passes the console-0 hand-off (open the gate, arm
///   the UART receive interrupt, resolve the `LateUsersDb` pending wait);
///   coupling it here makes forgetting it impossible.
/// * `supervisor` — the optional pre-boot Supervisor. When present, a bounded
///   ESC boot-screen window is drawn before anything mounts — even on a disk
///   that cannot be read, so the Supervisor can diagnose it — once console 0
///   can deliver a key ([`KeyArrival`]); a lone `ESC` drops into the
///   Supervisor REPL, and if the operator mounts the root from there the
///   unlock returns [`UnlockOutcome::Installed`] with no further prompt.
///   [`None`] (host tests) skips the window entirely.
///
/// Driver loading is **not** part of this policy: under design B the
/// user-space `devmgr` loads drivers over the driver-store endpoint served
/// off the read-only `/System` volume ([`with_system_volume`] /
/// [`crate::driver_store_server`]), independent of the root (user-data) mount
/// — the store volume is reachable without any passphrase.
// Each parameter is a distinct, irreducible boot seam the unlock threads
// through — the disk, the console write/read halves, the install cells, the
// audit sink, the anti-brute-force delay, the console-release callback, and
// the optional pre-boot Supervisor. Bundling them into a struct would
// only rename the same fields without removing any, so the argument list is
// the honest shape of the seam.
#[allow(clippy::too_many_arguments)]
pub fn unlock_root_disk_interactively<Disk: Block>(
    disk: Disk,
    console: &dyn ConsoleWrite,
    input: &dyn ConsoleRead,
    install: &UnlockInstall<'_>,
    audit: &dyn Sink,
    delay: &dyn Fn(),
    on_resolved: &dyn Fn(),
    supervisor: Option<SupervisorWindow<'_>>,
) -> UnlockOutcome {
    // Run the interactive unlock to a terminal outcome, then hand the
    // console back to `login` — *exactly once, on every outcome and every
    // internal return path*. Coupling the release to the resolution here,
    // rather than expecting each caller to remember it, is deliberate: the in-kernel unlock kthread owns console 0 for
    // the duration of the passphrase prompt (its `GatedConsoleRead` keeps
    // `login` parked), so the moment the unlock resolves — a database
    // installed *or* given up — the gate must open, the UART receive
    // interrupt must arm, and the `LateUsersDb` pending wait must resolve,
    // or no console could ever be typed into even though the root mounted.
    // A successful unlock previously skipped that release (only the
    // fail-closed branches ran it), wedging both the keyboard and serial
    // `login` after a good unlock; threading it through `on_resolved` makes
    // forgetting it impossible.
    let outcome = unlock_root_disk_interactively_impl(
        disk, console, input, install, audit, delay, supervisor,
    );
    on_resolved();
    outcome
}

/// The interactive-unlock state machine itself (see
/// [`unlock_root_disk_interactively`], which wraps this with the mandatory
/// console-release-on-resolution). Split out so the release cannot be
/// skipped by any of this function's internal return paths.
fn unlock_root_disk_interactively_impl<Disk: Block>(
    disk: Disk,
    console: &dyn ConsoleWrite,
    input: &dyn ConsoleRead,
    install: &UnlockInstall<'_>,
    audit: &dyn Sink,
    delay: &dyn Fn(),
    supervisor: Option<SupervisorWindow<'_>>,
) -> UnlockOutcome {
    // The disk is borrowed mutably for each attempt through the
    // `impl Block for &mut B` forwarding, so one device is reused across
    // retries without re-acquiring it.
    let mut disk = disk;
    // One host serves both entry points: the boot-screen window here and a
    // lone `ESC` at the live passphrase prompt below.
    let mut supervisor = supervisor;
    // The production cell the operator's one-boot login choice is recorded in;
    // `enter_supervisor` installs into it from either entry point.
    let boot_session = &LATE_BOOT_SESSION;
    // The window opens before the disk is read, so a disk slow to answer
    // cannot hold off the Supervisor that would diagnose it.
    let window = match supervisor.as_mut() {
        Some(supervisor) => match esc_boot_window(console, input, supervisor, boot_session) {
            EscWindow::Mounted => return UnlockOutcome::Installed,
            window => Some(window),
        },
        None => None,
    };

    let descriptor = match read_root_lock(&mut disk, audit) {
        Ok(RootLock::Passphrase(descriptor)) => descriptor,
        Ok(RootLock::Plaintext) => {
            return match mount_root_disk_and_load_users(&mut disk, RootUnlock::Plaintext, audit) {
                Ok(unlocked) => {
                    write_all(console, FS_UNLOCKED_LINE);
                    finish_install(unlocked, install, audit)
                }
                Err(error) => give_up(console, audit, error.cause()),
            };
        }
        // `read_root_lock` already audited the precise cause.
        Err(error) => return give_up(console, audit, error.cause()),
    };
    // A window that elapsed ends with the prompt drawn over its announcement,
    // and a byte it took is the first character of the first passphrase.
    let (mut prompt_drawn, mut initial_byte) = match window {
        Some(EscWindow::Elapsed { initial }) => {
            write_all(console, FS_UNLOCK_PROMPT_INPLACE);
            (true, initial)
        }
        _ => (false, None),
    };

    // A wrong typed passphrase is asked again, forever: the encrypted root
    // holds the only copy of the user data and login is refused until it
    // mounts, so there is no useful state to advance to without it. A wrong
    // attempt is rate-limited by `delay` (a minimum-duration pause the
    // production kthread backs with a timed park, never a busy-wait) before
    // the operator is told and re-prompted, so this is not a brute-force
    // oracle to spin against. Only a *structural* failure the disk itself
    // cannot satisfy, or a dead console, fails closed — re-prompting those
    // could never succeed.
    loop {
        // The ESC window already redrew the prompt in place of its
        // announcement, so the first interactive prompt must not draw a second
        // fresh line over it; every subsequent re-prompt draws normally.
        if prompt_drawn {
            prompt_drawn = false;
        } else {
            write_all(console, FS_UNLOCK_PROMPT);
        }

        // A zeroized, fixed-length buffer: the typed secret never reaches
        // the heap and is wiped when this attempt's buffer drops.
        let mut passphrase = Zeroizing::new([0u8; MAX_ROOT_PASSPHRASE_LEN]);
        // Only the *first* prompt inherits a byte the ESC window pre-read;
        // every re-prompt starts empty.
        let len = match read_passphrase_line(input, &mut passphrase[..], initial_byte.take()) {
            Ok(len) => len,
            Err(PassphraseReadError::TooLong) => {
                // An over-long line is a wrong attempt, not a fatal console
                // fault: treat it exactly like a wrong passphrase and prompt
                // again after the same delay.
                reject_and_wait(console, audit, delay);
                continue;
            }
            Err(PassphraseReadError::Console) => {
                // The console could not deliver a line: fail closed rather
                // than retry against a dead input.
                return give_up(console, audit, "console_unreadable");
            }
            Err(PassphraseReadError::Escape) => {
                // A lone `ESC` at the live prompt drops into the Supervisor
                // (when a host is threaded). If the operator mounts from
                // there the unlock is already resolved; otherwise the REPL
                // redrew the screen, so re-prompt on a fresh line. With no
                // host (host tests) an `ESC` is a no-op that simply
                // re-prompts.
                if let Some(supervisor) = supervisor.as_mut() {
                    match enter_supervisor(console, input, supervisor.host, boot_session) {
                        SupervisorExit::Mounted => return UnlockOutcome::Installed,
                        // Any session choice was recorded by
                        // `enter_supervisor`; re-prompt for the passphrase.
                        SupervisorExit::ContinueBoot(_) => {}
                    }
                }
                continue;
            }
        };

        let unlock = RootUnlock::Passphrase {
            descriptor: &descriptor,
            passphrase: &passphrase[..len],
        };
        match mount_root_disk_and_load_users(&mut disk, unlock, audit) {
            Ok(unlocked) => {
                // The passphrase was accepted: collapse the prompt line to
                // just the filesystem label (overwriting the typed line and
                // its completed-marker) and leave one blank line so the login
                // `Username:` prompt that follows is clearly separated.
                write_all(console, FS_UNLOCKED_LINE);
                return finish_install(unlocked, install, audit);
            }
            Err(RootMountError::Mount(DriverError::PermissionDenied)) => {
                // Wrong passphrase: the master key never unwrapped. Never an
                // oracle; wait out the rate-limit delay, tell the operator,
                // and ask again.
                reject_and_wait(console, audit, delay);
            }
            Err(error) => {
                // A structural failure (no table, no partition, an
                // unreadable/invalid descriptor, a corrupt database):
                // re-prompting cannot fix the disk, so give up now. The
                // mount step already audited the precise cause (`4134`);
                // record the give-up with the same secret-free cause.
                return give_up(console, audit, error.cause());
            }
        }
    }
}

/// A wrong (or over-long) passphrase was refused: end the completed-marker
/// line, audit the refusal, wait out the anti-brute-force `delay`, then tell
/// the operator the passphrase was wrong before the caller re-prompts.
///
/// The message is drawn **after** the delay, so a brute-force attacker
/// scripting the prompt gains no faster signal than the honest operator —
/// the pause is paid before any feedback appears. No passphrase byte is ever
/// written or logged.
fn reject_and_wait(console: &dyn ConsoleWrite, audit: &dyn Sink, delay: &dyn Fn()) {
    // Return the cursor to the start of a fresh line, off the
    // `[input complete]` marker the submitted line left on screen.
    write_all(console, b"\r\n");
    retry(audit);
    delay();
    write_all(console, b"Incorrect passphrase\r\n");
}

/// Write every byte of `bytes` to `console`, looping over short writes and
/// stopping on a zero-length write or an error.
///
/// Best effort: the prompt is advisory, so a console that cannot accept it
/// does not abort the unlock — the read still parks for input. Never spins
/// on a stalled device.
fn write_all(console: &dyn ConsoleWrite, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        match console.write(bytes) {
            Ok(0) | Err(_) => break,
            Ok(n) => bytes = &bytes[n.min(bytes.len())..],
        }
    }
}

/// Arms a console's secret-entry activity marker for the lifetime of the
/// guard, disarming it on drop.
///
/// A passphrase read brackets itself with one of these so the `[input active…]`
/// marker is shown only while the secret line is being typed and is cleared on
/// *every* exit path — a completed line, a too-long line, a console fault, or a
/// lone-`ESC` drop to the Supervisor. Because the disarm runs from [`Drop`], no
/// return path can leak an armed marker into an ordinary echoed read (the
/// pre-boot Supervisor REPL) that shares the same console reader. A backing
/// with no secret feedback (the host test consoles) treats both calls as
/// no-ops.
struct SecretScope<'a> {
    input: &'a dyn ConsoleRead,
}

impl<'a> SecretScope<'a> {
    fn new(input: &'a dyn ConsoleRead) -> Self {
        input.set_secret(true);
        Self { input }
    }
}

impl Drop for SecretScope<'_> {
    fn drop(&mut self) {
        self.input.set_secret(false);
    }
}

/// Read one passphrase line from `input` into `buf`, returning its length.
///
/// Reads byte by byte so the read never consumes past the line terminator
/// (`CR` or `LF`), which a one-shot prompt could not put back. Line editing
/// is the shared read line discipline (`tairix_vt::line::LineEditor` — the
/// one definition login and the shell also run): a Backspace or the Delete
/// key's `CSI 3 ~` sequence rubs out the previous byte (zeroing its slot),
/// so a mistyped passphrase can be corrected without a fresh attempt. A
/// line that fills `buf` before a terminator is drained to the newline and
/// reported as [`PassphraseReadError::TooLong`] rather than truncated.
///
/// # Errors
///
/// [`PassphraseReadError::Console`] if the device read fails, or signals
/// end of input before any byte of a line arrived (fail closed); [`PassphraseReadError::TooLong`] for an over-length
/// line.
fn read_passphrase_line(
    input: &dyn ConsoleRead,
    buf: &mut [u8],
    initial: Option<u8>,
) -> Result<usize, PassphraseReadError> {
    // Arm the console's secret-entry activity marker for the span of *this*
    // read only, so the `[input active…]` indicator appears while the operator
    // types a passphrase and is cleared the instant the line resolves — on
    // every return path, including the lone-`ESC` Supervisor drop below. The
    // guard's drop disarms it, so an echoed command prompt read through the
    // same console reader (the pre-boot Supervisor REPL) never inherits the
    // marker.
    let _secret = SecretScope::new(input);
    let mut editor = tairix_vt::line::LineEditor::new();
    let mut len = 0usize;
    // A byte the ESC boot window already consumed (the operator began typing
    // the passphrase before the window elapsed) is this line's first
    // character: fold it in before reading the rest, so a quickly-typed secret
    // is never corrupted by a lost leading byte. The window never forwards an
    // `ESC` (it resolves the Supervisor drop itself), so this byte is ordinary
    // line content and the lone-`ESC` handling below applies only to a byte
    // this function reads first-hand.
    if let Some(byte) = initial {
        match editor.push(buf, &mut len, byte) {
            tairix_vt::line::LineFeed::Pending | tairix_vt::line::LineFeed::Escape => {}
            tairix_vt::line::LineFeed::Complete => return Ok(len),
            tairix_vt::line::LineFeed::TooLong => {
                drain_to_newline(input);
                return Err(PassphraseReadError::TooLong);
            }
        }
    }
    loop {
        let mut byte = [0u8; 1];
        let read = input
            .read(&mut byte)
            .map_err(|_| PassphraseReadError::Console)?;
        if read == 0 {
            // End of input. A line with content is accepted as typed; an
            // empty one means the console closed with nothing — fail closed
            // rather than mount under an empty passphrase.
            return if len == 0 {
                Err(PassphraseReadError::Console)
            } else {
                Ok(len)
            };
        }
        // A lone `ESC` as the **first** byte of the line drops into the
        // Supervisor from the live prompt; an `ESC [ …` CSI editor sequence
        // is consumed and ignored (its bytes never land in the passphrase).
        // Mid-line, `ESC` is held inertly by the editor exactly as before.
        if len == 0 && byte[0] == 0x1b {
            if esc_is_lone_escape(input) {
                return Err(PassphraseReadError::Escape);
            }
            continue;
        }
        match editor.push(buf, &mut len, byte[0]) {
            // A bare mid-line `ESC` is held by the editor exactly as before;
            // the first-byte lone-`ESC` Supervisor drop is handled above.
            tairix_vt::line::LineFeed::Pending | tairix_vt::line::LineFeed::Escape => {}
            tairix_vt::line::LineFeed::Complete => return Ok(len),
            tairix_vt::line::LineFeed::TooLong => {
                drain_to_newline(input);
                return Err(PassphraseReadError::TooLong);
            }
        }
    }
}

/// Drain console input up to and including the next line terminator,
/// discarding it, so a subsequent read starts on a fresh line. Stops on a
/// terminator, a zero-length read, or an error (never spins).
fn drain_to_newline(input: &dyn ConsoleRead) {
    loop {
        let mut byte = [0u8; 1];
        match input.read(&mut byte) {
            Ok(0) | Err(_) => return,
            Ok(_) if byte[0] == b'\n' || byte[0] == b'\r' => return,
            Ok(_) => {}
        }
    }
}

/// Audit a wrong (or over-long) passphrase attempt that will be retried.
/// No passphrase byte is logged.
fn retry(audit: &dyn Sink) {
    log(
        audit,
        &Event {
            level: Level::Warn,
            id: ROOT_UNLOCK_RETRY,
            message: "root-unlock: passphrase refused; prompting again",
            fields: &[],
        },
    );
}

/// Give the root up for `cause`: say so on the line the boot screen left, so
/// the operator is told why login stays refused, and record it.
fn give_up(console: &dyn ConsoleWrite, audit: &dyn Sink, cause: &'static str) -> UnlockOutcome {
    write_all(console, FS_GAVE_UP_PREFIX);
    write_all(console, cause.as_bytes());
    write_all(console, b"\x1b[K\r\n");
    gave_up(audit, cause);
    UnlockOutcome::GaveUp
}

/// Audit a fail-closed give-up of the interactive unlock, naming the
/// secret-free `cause`. No database is installed.
fn gave_up(audit: &dyn Sink, cause: &'static str) {
    log(
        audit,
        &Event {
            level: Level::Error,
            id: ROOT_UNLOCK_GAVE_UP,
            message: "root-unlock: gave up; no users database installed (reboot required)",
            fields: &[Field {
                key: "cause",
                value: tairix_log::FieldValue::Str(cause),
            }],
        },
    );
}

/// How the root on `disk` is locked, as the Supervisor's `mount` reads it
/// before asking for anything: asking is not a decision, so nothing is
/// audited. `None` when the disk cannot answer.
pub(crate) fn supervisor_root_lock<Disk: Block>(disk: &mut Disk) -> Option<SupervisorRootLock> {
    match probe_root_lock(disk) {
        Ok(RootLock::Plaintext) => Some(SupervisorRootLock::Plaintext),
        Ok(RootLock::Passphrase(_)) => Some(SupervisorRootLock::Passphrase),
        Err(_) => None,
    }
}

/// The Supervisor's `mount`: the root on `disk` mounted as the normal boot
/// mounts it, with no key when plaintext and under `passphrase` when
/// encrypted, and published through the same `install` cells. A wrong
/// passphrase waits out `delay` before it is answered, as the boot prompt's
/// does, so the Supervisor is no faster an oracle.
pub(crate) fn supervisor_mount<Disk: Block>(
    disk: &mut Disk,
    passphrase: Option<&[u8]>,
    install: &UnlockInstall<'_>,
    audit: &dyn Sink,
    delay: &dyn Fn(),
    out: &mut dyn SupReport,
) -> MountOutcome {
    let lock = match read_root_lock(disk, audit) {
        Ok(lock) => lock,
        Err(error) => return cannot_mount(error, out),
    };
    let unlock = match (&lock, passphrase) {
        (RootLock::Plaintext, None) => RootUnlock::Plaintext,
        (RootLock::Passphrase(descriptor), Some(passphrase)) => RootUnlock::Passphrase {
            descriptor,
            passphrase,
        },
        // The disk is no longer locked the way the engine was told, so what
        // the operator was asked for no longer fits it.
        _ => {
            out.line("root cannot be mounted: its protection changed; run mount again");
            return MountOutcome::Failed;
        }
    };
    match mount_root_disk_and_load_users(disk, unlock, audit) {
        Ok(unlocked) => match finish_install(unlocked, install, audit) {
            UnlockOutcome::Installed => {
                out.line("root mounted");
                MountOutcome::Mounted
            }
            UnlockOutcome::GaveUp => {
                out.line("root mounted but the users database could not be installed");
                MountOutcome::Failed
            }
        },
        Err(RootMountError::Mount(DriverError::PermissionDenied)) => {
            delay();
            out.line("incorrect passphrase");
            MountOutcome::WrongPassphrase
        }
        Err(error) => cannot_mount(error, out),
    }
}

/// Render a refused Supervisor mount by its secret-free cause.
fn cannot_mount(error: RootMountError, out: &mut dyn SupReport) -> MountOutcome {
    out.write_str("root cannot be mounted: ");
    out.line(error.cause());
    MountOutcome::Failed
}

/// Classify a refused mount into the audit record it is logged as.
///
/// A *wrong passphrase* — `Mount(PermissionDenied)` — is an expected
/// fail-closed authentication non-match, not a system error: the derived key
/// never unwrapped the master key and there is no oracle either way. It maps to
/// the below-`Info` [`ROOT_UNLOCK_KEY_REJECTED`] so routine retries cannot
/// flood the boot log. Every other refusal is a genuine structural failure (a
/// corrupt, missing or contradicted descriptor, a missing/malformed partition
/// table or partition, a non-arxfs volume, a device fault) and maps to the
/// `Error`-level [`ROOT_MOUNT_REJECTED`].
///
/// A pure mapping so the audit level/event a refusal earns can be asserted
/// directly, without depending on the global log threshold.
fn rejection_record(error: RootMountError) -> (EventId, Level, &'static str) {
    match error {
        RootMountError::Mount(DriverError::PermissionDenied) => (
            ROOT_UNLOCK_KEY_REJECTED,
            Level::Debug,
            "root-mount: derived key did not unlock the volume (wrong passphrase)",
        ),
        _ => (
            ROOT_MOUNT_REJECTED,
            Level::Error,
            "root-mount: root volume mount refused; no users database served",
        ),
    }
}

/// Audit a refused unlock, naming the failing stage with a secret-free
/// cause string. The database load stage audits itself, so this helper
/// reports a partition-table parse, partition lookup/window, descriptor
/// read, descriptor decode, or mount refusal. The event and level are
/// chosen by [`rejection_record`]: a structural refusal is an `Error`, a
/// wrong passphrase a below-`Info` `Debug` (it is no error).
fn reject(audit: &dyn Sink, error: RootMountError) {
    let (id, level, message) = rejection_record(error);
    log(
        audit,
        &Event {
            level,
            id,
            message,
            fields: &[Field {
                key: "cause",
                value: tairix_log::FieldValue::Str(error.cause()),
            }],
        },
    );
}

/// Read the plaintext `root.unlock` key-derivation descriptor off the FAT
/// boot partition.
///
/// The boot path recovers this descriptor *before* anything is decrypted
/// ([`read_root_lock`]), and an encrypted root's key is derived through it
/// from the passphrase the operator types. Reading it through
/// the same real FAT32 driver that `tools/mkimage` / the installer
/// authored it with keeps one on-disk definition for both ends; the file name is the shared
/// [`ROOT_UNLOCK_NAME`] constant.
///
/// The descriptor is a fixed-length record ([`UNLOCK_DESCRIPTOR_LEN`]
/// bytes), so the read is strictly bounded and fail-closed: the entry's size is checked to be **exactly** that
/// length *before* a byte is read — rejecting both a truncated and an
/// over-long file — and the bytes are read into a fixed on-stack buffer.
/// The returned bytes are still untrusted: [`UnlockDescriptor::decode`]
/// (inside [`read_root_lock`]) validates every field before they drive any
/// key derivation.
///
/// * `boot_partition` — the FAT boot-partition [`Block`] device the board
///   brought up (the GPU-firmware-readable partition on a Pi SD card).
///
/// # Errors
///
/// A [`DriverError`] from the first check that refused: the partition does
/// not mount as FAT32, [`ROOT_UNLOCK_NAME`] is absent
/// ([`DriverError::NotFound`]), the entry is not a regular file
/// ([`DriverError::Unsupported`]), its size is not exactly
/// [`UNLOCK_DESCRIPTOR_LEN`] ([`DriverError::OutOfRange`]), or the device
/// read faulted. No partial descriptor is ever returned.
pub fn read_root_unlock_descriptor<B: Block>(
    boot_partition: B,
) -> Result<[u8; UNLOCK_DESCRIPTOR_LEN], DriverError> {
    let mut fs = Fat32::open(boot_partition)?;
    let root = fs.root();
    let node = fs.lookup(root, ROOT_UNLOCK_NAME.as_bytes())?;

    // Validate shape and size *before* reading: a fixed-length record, so
    // anything that is not exactly one is refused rather than partially
    // read (a format bound, not a capacity).
    let info = fs.node_info(node)?;
    if info.kind != NodeKind::RegularFile {
        return Err(DriverError::Unsupported);
    }
    let size = usize::try_from(info.size).map_err(|_| DriverError::OutOfRange)?;
    if size != UNLOCK_DESCRIPTOR_LEN {
        return Err(DriverError::OutOfRange);
    }

    let mut descriptor = [0u8; UNLOCK_DESCRIPTOR_LEN];
    let read = fs.read_at(node, 0, &mut descriptor)?;
    if read != UNLOCK_DESCRIPTOR_LEN {
        return Err(DriverError::DeviceFault);
    }
    Ok(descriptor)
}

#[cfg(test)]
mod tests {
    use super::*;

    use core::cell::RefCell;

    use alloc::vec::Vec;

    use tairix_abi::driver::block::BlockGeometry;
    use tairix_abi::driver::filesystem::FilesystemWrite;
    use tairix_abi::BootSession;
    use tairix_drv_fs_arxfs::{EntropySource, UNLOCK_MIN_ITERATIONS};
    use tairix_kernel_core::UsersDbSource;
    use tairix_log::{Event as LogEvent, Sink as LogSink};
    use tairix_partition::{mbr, Partition};
    use tairix_supervisor::{MountOutcome, SupervisorEvent, TestOutcome};
    use tairix_test_arxfs_image as image;
    use tairix_test_encrypted_root_image as disk_image;
    use tairix_users::UsersDb;

    /// Deterministic entropy for provisioning a descriptor's salt in
    /// tests. A fixed sequence keeps the test reproducible; it is test scaffolding, never a production source.
    struct SeqEntropy {
        next: u8,
    }

    impl EntropySource for SeqEntropy {
        fn fill(&mut self, out: &mut [u8]) -> Result<(), DriverError> {
            for byte in out.iter_mut() {
                *byte = self.next;
                self.next = self.next.wrapping_add(1);
            }
            Ok(())
        }
    }

    /// Records every audited event id so a test can assert the audit trail.
    struct RecordingSink {
        ids: RefCell<Vec<u32>>,
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                ids: RefCell::new(Vec::new()),
            }
        }
        fn ids(&self) -> Vec<u32> {
            self.ids.borrow().clone()
        }
    }

    impl LogSink for RecordingSink {
        fn write_event(&self, event: &LogEvent<'_>) {
            self.ids.borrow_mut().push(event.id.0);
        }
    }

    /// The passphrase the test "operator" types; the volume is provisioned
    /// under the key derived from it. Forwarded from the shared whole-disk
    /// fixture so the in-memory split tests and the `-M virt` QEMU vertical
    /// type one passphrase.
    const PASSPHRASE: &[u8] = disk_image::PASSPHRASE;

    /// An encoded descriptor at the policy floor (100k iterations), the
    /// cheapest a descriptor may carry, so the per-test PBKDF2 derivations
    /// stay bounded while still running the real key derivation.
    fn descriptor_bytes() -> [u8; UNLOCK_DESCRIPTOR_LEN] {
        let descriptor =
            UnlockDescriptor::provision(UNLOCK_MIN_ITERATIONS, &mut SeqEntropy { next: 7 })
                .expect("descriptor provisions");
        let mut bytes = [0u8; UNLOCK_DESCRIPTOR_LEN];
        descriptor.encode(&mut bytes).expect("descriptor encodes");
        bytes
    }

    #[test]
    fn a_wrong_passphrase_is_classified_below_error_every_structural_refusal_is_an_error() {
        // A wrong passphrase is an expected fail-closed authentication
        // non-match, recorded below the default `Info` filter so routine
        // retries cannot flood the boot log, while every structural refusal
        // stays an ERROR the operator sees. Asserting the mapping directly is
        // independent of the global log threshold another test may lower.
        assert_eq!(
            rejection_record(RootMountError::Mount(DriverError::PermissionDenied)),
            (
                ROOT_UNLOCK_KEY_REJECTED,
                Level::Debug,
                "root-mount: derived key did not unlock the volume (wrong passphrase)"
            )
        );
        assert!(Level::Debug < Level::Info);

        // Every other refusal — including a non-arxfs volume, a protection the
        // mount did not expect, or a device fault, which are *also* `Mount(_)`
        // but not a wrong passphrase — is the ERROR-level structural rejection.
        for error in [
            RootMountError::Mount(DriverError::BadMagic),
            RootMountError::Mount(DriverError::Unsupported),
            RootMountError::Mount(DriverError::DeviceFault),
            RootMountError::DescriptorRead(DriverError::NotFound),
            RootMountError::Descriptor(DriverError::BadMagic),
            RootMountError::DescriptorBesidePlaintextRoot,
            RootMountError::NoBootPartition,
            RootMountError::NoRootPartition,
            RootMountError::PartitionWindow(DriverError::OutOfRange),
        ] {
            let (id, level, _) = rejection_record(error);
            assert_eq!(id, ROOT_MOUNT_REJECTED, "{error:?}");
            assert_eq!(level, Level::Error, "{error:?}");
        }
    }

    // --- read_root_unlock_descriptor ----------------------------------

    /// The whole-disk fixture's sector size, which the FAT boot partition
    /// is addressed in.
    const FAT_SECTOR_BYTES: usize = disk_image::SECTOR_BYTES;

    /// 64 MiB, the production boot-partition size `tools/mkimage` formats.
    /// A valid FAT32 volume needs far more than the small arxfs fixture's
    /// 2048 sectors, so this `Block` double is sized for a real FAT32 format
    /// rather than reusing `image::VecBlock` (whose fixed geometry is the
    /// arxfs fixture's). Forwarded from the shared whole-disk fixture so
    /// the boot-partition size has one definition.
    const FAT_BOOT_SECTORS: u64 = disk_image::BOOT_PART_SECTORS;

    /// In-memory FAT boot-partition [`Block`] double: a `Vec<u8>` addressed
    /// in [`FAT_SECTOR_BYTES`]-byte sectors, exactly as the board's
    /// SD/virtio-blk device presents the partition. Sized from its backing
    /// length so a real FAT32 volume formats on it.
    struct FatVecBlock {
        store: Vec<u8>,
    }

    impl FatVecBlock {
        fn new(sectors: u64) -> Self {
            let len = usize::try_from(sectors).expect("sector count fits usize") * FAT_SECTOR_BYTES;
            Self {
                store: alloc::vec![0u8; len],
            }
        }

        fn span(&self, lba: u64, len: usize) -> Result<(usize, usize), DriverError> {
            if len == 0 || !len.is_multiple_of(FAT_SECTOR_BYTES) {
                return Err(DriverError::BufferTooSmall);
            }
            let start = usize::try_from(lba)
                .ok()
                .and_then(|l| l.checked_mul(FAT_SECTOR_BYTES))
                .ok_or(DriverError::LengthOutOfRange)?;
            let end = start
                .checked_add(len)
                .ok_or(DriverError::LengthOutOfRange)?;
            if end > self.store.len() {
                return Err(DriverError::LengthOutOfRange);
            }
            Ok((start, end))
        }
    }

    impl Block for FatVecBlock {
        fn geometry(&self) -> Result<BlockGeometry, DriverError> {
            Ok(BlockGeometry {
                block_size: u32::try_from(FAT_SECTOR_BYTES).expect("sector size fits u32"),
                block_count: u64::try_from(self.store.len() / FAT_SECTOR_BYTES)
                    .expect("sector count fits u64"),
            })
        }

        fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
            let (start, end) = self.span(lba, buf.len())?;
            buf.copy_from_slice(&self.store[start..end]);
            Ok(())
        }

        fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
            let (start, end) = self.span(lba, buf.len())?;
            self.store[start..end].copy_from_slice(buf);
            Ok(())
        }

        fn flush(&mut self) -> Result<(), DriverError> {
            Ok(())
        }
    }

    /// Author a FAT boot partition through the real FAT32 driver and plant
    /// `payload` under [`ROOT_UNLOCK_NAME`] — the exact write `tools/mkimage`
    /// performs. Returns the mounted-and-flushed device,
    /// ready to hand straight to [`read_root_unlock_descriptor`].
    fn author_boot_partition(payload: &[u8]) -> FatVecBlock {
        let mut fs =
            Fat32::format(FatVecBlock::new(FAT_BOOT_SECTORS), 0x0B00_7001).expect("FAT32 formats");
        let root = fs.root();
        fs.create(root, ROOT_UNLOCK_NAME.as_bytes(), NodeKind::RegularFile)
            .expect("the descriptor file is created");
        let written = fs
            .write_at(root, ROOT_UNLOCK_NAME.as_bytes(), 0, payload)
            .expect("the descriptor bytes are written");
        assert_eq!(written, payload.len(), "the whole descriptor is written");
        fs.flush().expect("the partition flushes");
        fs.into_block()
    }

    /// An empty FAT boot partition carrying no `root.unlock` file.
    fn empty_boot_partition() -> FatVecBlock {
        let mut fs =
            Fat32::format(FatVecBlock::new(FAT_BOOT_SECTORS), 0x0B00_7002).expect("FAT32 formats");
        fs.flush().expect("the partition flushes");
        fs.into_block()
    }

    #[test]
    fn reads_back_the_exact_planted_descriptor() {
        // The on-FAT descriptor round-trips byte-for-byte through the real
        // driver and decodes to the same descriptor it was provisioned as
        // (author and reader share one definition).
        let descriptor_bytes = descriptor_bytes();
        let block = author_boot_partition(&descriptor_bytes);

        let read = read_root_unlock_descriptor(block).expect("the planted descriptor reads back");
        assert_eq!(read, descriptor_bytes);
        // The bytes are a genuinely well-formed descriptor.
        UnlockDescriptor::decode(&read).expect("the read descriptor decodes");
    }

    #[test]
    fn a_missing_descriptor_is_not_found() {
        // no `root.unlock` on the partition is a fail-closed
        // NotFound, never a fabricated descriptor.
        let block = empty_boot_partition();
        assert_eq!(
            read_root_unlock_descriptor(block),
            Err(DriverError::NotFound)
        );
    }

    #[test]
    fn a_truncated_descriptor_is_refused() {
        // a short file is rejected on its size *before* any
        // read, never zero-padded up to the record length.
        let block = author_boot_partition(&descriptor_bytes()[..UNLOCK_DESCRIPTOR_LEN - 1]);
        assert_eq!(
            read_root_unlock_descriptor(block),
            Err(DriverError::OutOfRange)
        );
    }

    #[test]
    fn an_over_long_descriptor_is_refused() {
        // a file longer than the fixed record — extra bytes a
        // tampered partition might append — is refused, not truncated to
        // the prefix.
        let mut padded = descriptor_bytes().to_vec();
        padded.push(0);
        let block = author_boot_partition(&padded);
        assert_eq!(
            read_root_unlock_descriptor(block),
            Err(DriverError::OutOfRange)
        );
    }

    #[test]
    fn an_unformatted_partition_does_not_mount() {
        // a device that is not a FAT volume (a zeroed image) fails
        // the mount closed rather than being misread.
        let block = FatVecBlock::new(FAT_BOOT_SECTORS);
        assert!(
            read_root_unlock_descriptor(block).is_err(),
            "an unformatted partition must not mount"
        );
    }

    // --- read_root_lock + mount_root_disk_and_load_users -----------------

    /// The whole-disk fixture with an encrypted root, provisioned under
    /// [`PASSPHRASE`] — the image the `-M virt` root-mount->login QEMU vertical
    /// plants on its virtio-blk backing.
    fn success_disk() -> FatVecBlock {
        let bytes = disk_image::build_image().expect("the whole-disk image assembles");
        FatVecBlock { store: bytes }
    }

    /// The whole-disk fixture with a plaintext root, as a shipped image has.
    fn plaintext_disk() -> FatVecBlock {
        let bytes = disk_image::build_plaintext_image().expect("the plaintext image assembles");
        FatVecBlock { store: bytes }
    }

    #[test]
    fn the_root_mount_finds_the_image_s_machine_configuration_on_the_system_volume() {
        let which = tairix_abi::driver_store::SystemConfigFile::System;
        let components: alloc::vec::Vec<&[u8]> =
            which.volume_path().split('/').map(str::as_bytes).collect();
        let shipped: &[u8] = b"cache.block off\n";
        let bytes = disk_image::build_image_with_contents(
            &[],
            &[(&components, shipped)],
            &[],
            disk_image::RootProtection::Plaintext,
        )
        .expect("the whole-disk image assembles");
        let mut disk = FatVecBlock { store: bytes };
        let table = parse_partition_table(&mut disk).expect("the table parses");
        let read = super::image_system_config(&mut disk, &table);
        assert!(
            matches!(&read, Ok(Some(found)) if found.as_slice() == shipped),
            "{read:?}"
        );

        let mut bare = plaintext_disk();
        let table = parse_partition_table(&mut bare).expect("the table parses");
        assert!(matches!(
            super::image_system_config(&mut bare, &table),
            Ok(None)
        ));
    }

    /// The bytes `ty`'s partition occupies on `disk`.
    fn partition_range(disk: &mut FatVecBlock, ty: PartitionType) -> core::ops::Range<usize> {
        let extent = parse_partition_table(disk)
            .expect("the fixture's table parses")
            .first_of_type(ty)
            .expect("the fixture carries the partition");
        let byte = |lba: u64| usize::try_from(lba).expect("fits") * FAT_SECTOR_BYTES;
        byte(extent.start_lba)..byte(extent.start_lba + extent.block_count)
    }

    /// `disk` with its boot partition replaced by `boot`.
    fn with_boot_partition(mut disk: FatVecBlock, boot: &FatVecBlock) -> FatVecBlock {
        let range = partition_range(&mut disk, PartitionType::FatBoot);
        disk.store[range].copy_from_slice(&boot.store);
        disk
    }

    /// Mount `disk` as the boot path does: read how its root is locked, then
    /// mount it — under `passphrase` when it takes one.
    fn mount_disk(
        disk: &mut FatVecBlock,
        passphrase: &[u8],
        sink: &RecordingSink,
    ) -> Result<UnlockedRoot, RootMountError> {
        let lock = read_root_lock(disk, sink)?;
        let unlock = match &lock {
            RootLock::Plaintext => RootUnlock::Plaintext,
            RootLock::Passphrase(descriptor) => RootUnlock::Passphrase {
                descriptor,
                passphrase,
            },
        };
        mount_root_disk_and_load_users(disk, unlock, sink)
    }

    /// The served database is the fixture's exact canonical `users-v1` text,
    /// authenticating its planted account and refusing a wrong password.
    fn assert_usable_database(root: &UnlockedRoot) {
        let text = root.users.text().expect("a loaded holder serves its text");
        let serialised = image::users_db_text().expect("fixture text serialises");
        assert_eq!(text, serialised.as_bytes());
        let db = UsersDb::parse(core::str::from_utf8(&text).expect("utf-8"))
            .expect("the served database parses");
        db.authenticate(
            image::USERS_FIXTURE_USERNAME,
            image::USERS_FIXTURE_PASSWORD.as_bytes(),
        )
        .expect("the planted account authenticates");
        assert!(db
            .authenticate(image::USERS_FIXTURE_USERNAME, b"wrong")
            .is_err());
    }

    #[test]
    fn the_correct_passphrase_unlocks_an_encrypted_root_and_loads_a_usable_database() {
        // One whole-disk device split by role into bounds-checked windows:
        // the descriptor is read off the FAT boot window and the encrypted
        // root window mounts under the passphrase (`plans/PI.md` P11 Chunk
        // B-2).
        let sink = RecordingSink::new();
        let root = mount_disk(&mut success_disk(), PASSPHRASE, &sink)
            .expect("the correct passphrase unlocks the root");
        assert_usable_database(&root);
        assert!(matches!(root.keying, RootKeying::Key(_)));
        assert!(sink.ids().contains(&4133), "{:?}", sink.ids());
        // The `/System` mount is the separate `with_system_volume` seam.
        assert!(!sink.ids().contains(&4140), "{:?}", sink.ids());
    }

    #[test]
    fn a_plaintext_root_mounts_with_no_key_and_loads_a_usable_database() {
        let sink = RecordingSink::new();
        let mut disk = plaintext_disk();
        assert_eq!(read_root_lock(&mut disk, &sink), Ok(RootLock::Plaintext));
        let root = mount_root_disk_and_load_users(&mut disk, RootUnlock::Plaintext, &sink)
            .expect("a plaintext root mounts with no key");
        assert_usable_database(&root);
        assert!(matches!(root.keying, RootKeying::Plaintext));
        assert!(sink.ids().contains(&4133), "{:?}", sink.ids());
        assert!(!sink.ids().contains(&4134), "{:?}", sink.ids());
    }

    #[test]
    fn a_wrong_passphrase_is_refused_fail_closed_with_no_oracle() {
        // A different passphrase derives a key that never unwraps the master
        // key, so the mount is refused and no database is served. It is an
        // authentication non-match, not a system error: never the ERROR-level
        // structural rejection (4134), but the below-`Info` 4142, dropped at
        // the default threshold and kept for forensics when it is lowered.
        let sink = RecordingSink::new();
        let err = mount_disk(&mut success_disk(), b"wrong passphrase", &sink)
            .expect_err("a wrong passphrase must be refused");
        assert_eq!(err, RootMountError::Mount(DriverError::PermissionDenied));
        assert_eq!(err.cause(), "unlock_refused");
        assert_eq!(rejection_record(err).0, ROOT_UNLOCK_KEY_REJECTED);
        assert!(!sink.ids().contains(&4134), "{:?}", sink.ids());
        assert!(!sink.ids().contains(&4133), "{:?}", sink.ids());
    }

    #[test]
    fn an_empty_or_over_long_passphrase_is_refused_as_a_wrong_one() {
        // No root is provisioned under either, so neither is a passphrase
        // that could be right; both are refused exactly as a wrong one is.
        let mut disk = success_disk();
        let over_long = [b'a'; MAX_ROOT_PASSPHRASE_LEN + 1];
        for passphrase in [b"" as &[u8], &over_long] {
            let sink = RecordingSink::new();
            let err = mount_disk(&mut disk, passphrase, &sink).expect_err("refused");
            assert_eq!(err, RootMountError::Mount(DriverError::PermissionDenied));
            assert!(!sink.ids().contains(&4134), "{:?}", sink.ids());
            assert!(!sink.ids().contains(&4133), "{:?}", sink.ids());
        }
    }

    #[test]
    fn a_tampered_descriptor_is_refused_before_any_key_derivation() {
        // The descriptor is plaintext and untrusted: a corrupt one is refused
        // outright, before the passphrase is consulted.
        let mut disk = success_disk();
        let boot = partition_range(&mut disk, PartitionType::FatBoot);
        let planted = read_root_unlock_descriptor(FatVecBlock {
            store: disk.store[boot.clone()].to_vec(),
        })
        .expect("the descriptor reads");
        let at = disk.store[boot.clone()]
            .windows(planted.len())
            .position(|window| window == planted)
            .expect("the descriptor lies on the boot partition");
        disk.store[boot.start + at] ^= 0xFF;
        let sink = RecordingSink::new();

        let err = mount_disk(&mut disk, PASSPHRASE, &sink)
            .expect_err("a tampered descriptor must be refused");

        assert!(matches!(err, RootMountError::Descriptor(_)), "{err:?}");
        assert_eq!(err.cause(), "descriptor_invalid");
        assert!(sink.ids().contains(&4134), "{:?}", sink.ids());
        assert!(!sink.ids().contains(&4133), "{:?}", sink.ids());
    }

    #[test]
    fn a_non_arxfs_root_is_refused() {
        let mut disk = success_disk();
        let root = partition_range(&mut disk, PartitionType::ARXFSRoot);
        disk.store[root].fill(0);
        let sink = RecordingSink::new();

        let err =
            mount_disk(&mut disk, PASSPHRASE, &sink).expect_err("a non-arxfs root must be refused");

        assert_eq!(err, RootMountError::Mount(DriverError::BadMagic));
        assert_eq!(err.cause(), "not_a_arxfs_volume");
        assert!(sink.ids().contains(&4134), "{:?}", sink.ids());
        assert!(!sink.ids().contains(&4133), "{:?}", sink.ids());
    }

    #[test]
    fn an_encrypted_root_with_no_descriptor_is_refused() {
        // Without its descriptor an encrypted root cannot be unlocked, so the
        // disk is refused rather than prompted for.
        let mut disk = with_boot_partition(success_disk(), &empty_boot_partition());
        let sink = RecordingSink::new();

        let err = mount_disk(&mut disk, PASSPHRASE, &sink)
            .expect_err("an encrypted root with no descriptor must be refused");

        assert_eq!(err, RootMountError::DescriptorRead(DriverError::NotFound));
        assert_eq!(err.cause(), "descriptor_unreadable");
        assert!(sink.ids().contains(&4134), "{:?}", sink.ids());
        assert!(!sink.ids().contains(&4133), "{:?}", sink.ids());
    }

    #[test]
    fn a_descriptor_beside_a_plaintext_root_is_refused() {
        // The boot partition promises a protection the root does not have:
        // refused, never mounted without the protection it claims.
        let mut disk = with_boot_partition(
            plaintext_disk(),
            &author_boot_partition(&descriptor_bytes()),
        );
        let sink = RecordingSink::new();

        let err = mount_disk(&mut disk, PASSPHRASE, &sink)
            .expect_err("a descriptor beside a plaintext root must be refused");

        assert_eq!(err, RootMountError::DescriptorBesidePlaintextRoot);
        assert_eq!(err.cause(), "descriptor_beside_plaintext_root");
        assert!(sink.ids().contains(&4134), "{:?}", sink.ids());
        assert!(!sink.ids().contains(&4133), "{:?}", sink.ids());
    }

    #[test]
    fn a_root_mounted_as_the_other_protection_is_refused() {
        // The mount is held to the protection the volume has, whatever its
        // caller expected.
        let sink = RecordingSink::new();
        let descriptor = UnlockDescriptor::decode(&descriptor_bytes()).expect("decodes");
        let as_encrypted = RootUnlock::Passphrase {
            descriptor: &descriptor,
            passphrase: PASSPHRASE,
        };
        assert_eq!(
            mount_root_disk_and_load_users(&mut plaintext_disk(), as_encrypted, &sink).err(),
            Some(RootMountError::Mount(DriverError::Unsupported))
        );
        assert_eq!(
            mount_root_disk_and_load_users(&mut success_disk(), RootUnlock::Plaintext, &sink).err(),
            Some(RootMountError::Mount(DriverError::PermissionDenied))
        );
        assert!(!sink.ids().contains(&4133), "{:?}", sink.ids());
    }

    #[test]
    fn a_disk_with_no_partition_table_serves_no_database() {
        // A device with no recognised partition scheme is refused whole by
        // both entries, and nothing is mounted.
        let sink = RecordingSink::new();
        let err = read_root_lock(&mut FatVecBlock::new(64), &sink)
            .expect_err("a disk with no table has no lock to read");
        assert!(matches!(err, RootMountError::PartitionTable(_)), "{err:?}");
        assert_eq!(err.cause(), "partition_table_invalid");
        let err =
            mount_root_disk_and_load_users(FatVecBlock::new(64), RootUnlock::Plaintext, &sink)
                .expect_err("a disk with no table must be refused");
        assert!(matches!(err, RootMountError::PartitionTable(_)), "{err:?}");
        assert_eq!(sink.ids().iter().filter(|&&id| id == 4134).count(), 2);
        assert!(!sink.ids().contains(&4133), "{:?}", sink.ids());
    }

    #[test]
    fn a_disk_without_a_root_partition_serves_no_database() {
        // A valid table that carries a FAT boot partition but no ARXFS root
        // partition is fail-closed NoRootPartition: there is no root to mount.
        let boot = author_boot_partition(&descriptor_bytes());
        let total_sectors = disk_image::BOOT_PART_LBA + FAT_BOOT_SECTORS;
        let mut store =
            alloc::vec![0u8; usize::try_from(total_sectors).expect("fits") * FAT_SECTOR_BYTES];
        let table = mbr::encode(&[Partition {
            ty: PartitionType::FatBoot,
            start_lba: disk_image::BOOT_PART_LBA,
            block_count: FAT_BOOT_SECTORS,
        }])
        .expect("the boot-only MBR encodes");
        store[..FAT_SECTOR_BYTES].copy_from_slice(&table);
        let boot_at = usize::try_from(disk_image::BOOT_PART_LBA).expect("fits") * FAT_SECTOR_BYTES;
        store[boot_at..boot_at + boot.store.len()].copy_from_slice(&boot.store);
        let mut disk = FatVecBlock { store };
        let sink = RecordingSink::new();

        let err = mount_disk(&mut disk, PASSPHRASE, &sink)
            .expect_err("a disk with no root partition must be refused");

        assert_eq!(err, RootMountError::NoRootPartition);
        assert_eq!(err.cause(), "no_root_partition");
        assert!(sink.ids().contains(&4134), "{:?}", sink.ids());
        assert!(!sink.ids().contains(&4133), "{:?}", sink.ids());
    }

    // --- unlock_root_disk_interactively (the prompt + retry policy) -----

    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use tairix_abi::Errno;

    /// A console byte sink that accepts everything written to it, so the
    /// prompt never short-writes. The interactive tests assert the audit
    /// trail and the installed state, not the prompt bytes, so a counting
    /// sink would add nothing.
    struct AcceptConsole;

    impl ConsoleWrite for AcceptConsole {
        fn write(&self, bytes: &[u8]) -> Result<usize, Errno> {
            Ok(bytes.len())
        }
    }

    /// A console sink that records every byte written, so a test can assert
    /// the prompt wording, the `Incorrect passphrase` notice, and the
    /// carriage-return / blank-line framing the interactive unlock emits.
    /// `Sync` through a `SpinLock`, as [`ConsoleWrite`] requires.
    struct RecordingConsole {
        written: tairix_sync::SpinLock<Vec<u8>>,
    }

    impl RecordingConsole {
        fn new() -> Self {
            Self {
                written: tairix_sync::SpinLock::new(Vec::new()),
            }
        }

        /// The recorded output as an owned byte vector.
        fn bytes(&self) -> Vec<u8> {
            self.written.lock().clone()
        }
    }

    impl ConsoleWrite for RecordingConsole {
        fn write(&self, bytes: &[u8]) -> Result<usize, Errno> {
            self.written.lock().extend_from_slice(bytes);
            Ok(bytes.len())
        }
    }

    /// Count the non-overlapping occurrences of `needle` in `haystack`.
    fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
        if needle.is_empty() || haystack.len() < needle.len() {
            return 0;
        }
        let mut count = 0;
        let mut i = 0;
        while i + needle.len() <= haystack.len() {
            if &haystack[i..i + needle.len()] == needle {
                count += 1;
                i += needle.len();
            } else {
                i += 1;
            }
        }
        count
    }

    /// A scripted console input source: yields the planted bytes one at a
    /// time (matching the byte-by-byte line reader), reports end of input
    /// (`Ok(0)`) once they are spent, and — if `fail_at` is set — fails the
    /// read once the cursor reaches it, modelling a device fault. `Sync`
    /// through an atomic cursor over immutable bytes, as [`ConsoleRead`]
    /// requires.
    struct ScriptInput {
        bytes: Vec<u8>,
        cursor: AtomicUsize,
        fail_at: Option<usize>,
        /// The reader's current secret-entry state, as last set through
        /// [`ConsoleRead::set_secret`]; the passphrase read arms it and every
        /// exit path must clear it again.
        secret: AtomicBool,
        /// Latches `true` the first time the reader is armed, so a test can
        /// prove the passphrase read *did* arm the marker (not merely that it
        /// ended cleared).
        secret_ever_armed: AtomicBool,
    }

    impl ScriptInput {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes,
                cursor: AtomicUsize::new(0),
                fail_at: None,
                secret: AtomicBool::new(false),
                secret_ever_armed: AtomicBool::new(false),
            }
        }

        fn failing() -> Self {
            Self {
                bytes: Vec::new(),
                cursor: AtomicUsize::new(0),
                fail_at: Some(0),
                secret: AtomicBool::new(false),
                secret_ever_armed: AtomicBool::new(false),
            }
        }
    }

    impl ConsoleRead for ScriptInput {
        fn read(&self, buf: &mut [u8]) -> Result<usize, Errno> {
            if buf.is_empty() {
                return Ok(0);
            }
            let i = self.cursor.load(Ordering::Relaxed);
            if self.fail_at == Some(i) {
                return Err(Errno::NotImplemented);
            }
            if i >= self.bytes.len() {
                return Ok(0);
            }
            buf[0] = self.bytes[i];
            self.cursor.store(i + 1, Ordering::Relaxed);
            Ok(1)
        }

        fn input_ready(&self) -> bool {
            let i = self.cursor.load(Ordering::Relaxed);
            self.fail_at == Some(i) || i < self.bytes.len()
        }

        fn set_secret(&self, secret: bool) {
            self.secret.store(secret, Ordering::Relaxed);
            if secret {
                self.secret_ever_armed.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Join `lines` into a newline-terminated byte script the reader
    /// consumes one passphrase per line.
    fn script(lines: &[&[u8]]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for line in lines {
            bytes.extend_from_slice(line);
            bytes.push(b'\n');
        }
        bytes
    }

    /// A fresh identity cell pre-loaded with the compiled-in system
    /// identity, exactly as the boot sec phase installs it before any
    /// unlock runs — the state `finish_install`'s replace-publish
    /// requires.
    fn boot_identity() -> LateIdentity {
        let cell = LateIdentity::new();
        let table = tairix_kernel_core::system_identity_table(&RecordingSink::new())
            .expect("the compiled system identity builds");
        cell.install(table)
            .expect("a fresh cell accepts the boot table");
        cell
    }

    #[test]
    fn the_esc_announcement_opens_a_fresh_line_for_the_blank_line_gap() {
        // The boot screen must show one blank line between the machine-summary
        // banner and the passphrase prompt. The announcement (which the
        // in-place prompt overwrites in situ) carries that spacing by opening a
        // fresh line, exactly as the standalone prompt does — so a regression
        // that drops the leading newline (and with it the blank line) fails
        // here.
        assert!(
            SUPERVISOR_ANNOUNCE.starts_with(b"\r\n"),
            "the announcement opens a fresh line so a blank line precedes it: {SUPERVISOR_ANNOUNCE:?}",
        );
        assert!(
            FS_UNLOCK_PROMPT.starts_with(b"\r\n"),
            "the standalone prompt draws the same one-blank-line spacing",
        );
    }

    #[test]
    fn a_passphrase_read_arms_the_secret_marker_only_across_its_own_read() {
        // The `[input active…]` marker must appear only while a passphrase line
        // is being typed. `read_passphrase_line` arms the reader for the span
        // of its read and clears it on return, so an echoed command prompt read
        // through the same console reader (the pre-boot Supervisor REPL) never
        // inherits the marker. A regression that armed the marker for the whole
        // unlock window (or forgot to clear it) leaves `secret` set here.
        let input = ScriptInput::new(script(&[PASSPHRASE]));
        let mut buf = [0u8; MAX_ROOT_PASSPHRASE_LEN];
        let len =
            read_passphrase_line(&input, &mut buf[..], None).expect("the scripted line reads");
        assert_eq!(len, PASSPHRASE.len(), "the whole passphrase was read");
        assert!(
            input.secret_ever_armed.load(Ordering::Relaxed),
            "the marker was armed while the passphrase was typed",
        );
        assert!(
            !input.secret.load(Ordering::Relaxed),
            "the marker is cleared again the instant the read resolves",
        );
    }

    #[test]
    fn a_byte_pre_read_by_the_esc_window_leads_the_passphrase_line() {
        // The ESC boot window consumes one byte while it waits. If the operator
        // began typing the passphrase before the window elapsed, that byte is
        // the line's first character and must be folded into the read — never
        // discarded, which would silently corrupt the secret and doom the
        // unlock to endless wrong-passphrase retries (the boot-hang defect).
        let first = PASSPHRASE[0];
        let rest = ScriptInput::new(script(&[&PASSPHRASE[1..]]));
        let mut buf = [0u8; MAX_ROOT_PASSPHRASE_LEN];
        let len = read_passphrase_line(&rest, &mut buf[..], Some(first))
            .expect("the remainder of the line reads");
        assert_eq!(
            &buf[..len],
            PASSPHRASE,
            "the pre-read byte leads the reconstructed passphrase, nothing lost",
        );
    }

    #[test]
    fn a_lone_escape_clears_the_secret_marker_before_the_supervisor_drop() {
        // A lone `ESC` at the live prompt drops to the Supervisor. The marker
        // must be cleared before the (echoed) REPL runs, so it does not paint
        // over the REPL's own echo — the drop-guard clears it on this early
        // return exactly as on a completed line.
        let input = ScriptInput::new(alloc::vec![0x1b]);
        let mut buf = [0u8; MAX_ROOT_PASSPHRASE_LEN];
        let outcome = read_passphrase_line(&input, &mut buf[..], None);
        assert!(
            matches!(outcome, Err(PassphraseReadError::Escape)),
            "a lone ESC as the first byte requests the Supervisor drop",
        );
        assert!(
            !input.secret.load(Ordering::Relaxed),
            "the marker is cleared before the Supervisor REPL runs",
        );
    }

    #[test]
    fn the_typed_passphrase_unlocks_and_installs_into_the_late_cell() {
        // The whole interactive path: the operator types the correct
        // passphrase at the first prompt, the disk unlocks, and the loaded
        // database is published into the set-once cell so login can
        // authenticate (`plans/PI.md` P11 Chunk B-2).
        let input = ScriptInput::new(script(&[PASSPHRASE]));
        let late = LateUsersDb::new();
        let late_identity = boot_identity();
        let storage_gid = LateStorageGid::new();
        let sink = RecordingSink::new();

        let outcome = unlock_root_disk_interactively(
            success_disk(),
            &AcceptConsole,
            &input,
            &UnlockInstall {
                users: &late,
                identity: &late_identity,
                groups: &LateGroupsDb::new(),
                writable: &NoWritableRootSink,
                admin: None,
                storage_gid: &storage_gid,
            },
            &sink,
            &|| {},
            &|| {},
            None,
        );

        assert_eq!(outcome, UnlockOutcome::Installed);
        assert!(late.is_installed(), "the database is published");
        // The fixture registry carries the well-known removable-storage
        // group; the unlock resolves it by name and arms the identity-map
        // policy cell with its gid.
        assert_eq!(
            storage_gid.get(),
            Some(GroupId(tairix_users::STORAGE_GID.0)),
            "the storage-group gid is resolved and installed"
        );
        let text = late
            .text()
            .expect("the cell now serves the loaded database");
        let db = UsersDb::parse(core::str::from_utf8(&text).expect("utf-8"))
            .expect("the served database parses");
        db.authenticate(
            image::USERS_FIXTURE_USERNAME,
            image::USERS_FIXTURE_PASSWORD.as_bytes(),
        )
        .expect("the planted account authenticates through the installed cell");
        assert!(
            sink.ids().contains(&4136),
            "install audited: {:?}",
            sink.ids()
        );
        assert!(
            !sink.ids().contains(&4137),
            "no retry on a first-try success: {:?}",
            sink.ids()
        );
    }

    #[test]
    fn a_plaintext_root_mounts_with_no_prompt() {
        // A shipped image's root is plaintext: it mounts with no console read
        // and no prompt drawn. Every read fails, so a mount proves none was
        // attempted.
        let late = LateUsersDb::new();
        let late_identity = boot_identity();
        let sink = RecordingSink::new();
        let console = RecordingConsole::new();

        let outcome = unlock_root_disk_interactively(
            plaintext_disk(),
            &console,
            &ScriptInput::failing(),
            &UnlockInstall {
                users: &late,
                identity: &late_identity,
                groups: &LateGroupsDb::new(),
                writable: &NoWritableRootSink,
                admin: None,
                storage_gid: &LateStorageGid::new(),
            },
            &sink,
            &|| {},
            &|| {},
            None,
        );

        assert_eq!(outcome, UnlockOutcome::Installed);
        let text = late.text().expect("the cell serves the loaded database");
        let db = UsersDb::parse(core::str::from_utf8(&text).expect("utf-8"))
            .expect("the served database parses");
        db.authenticate(
            image::USERS_FIXTURE_USERNAME,
            image::USERS_FIXTURE_PASSWORD.as_bytes(),
        )
        .expect("the planted account authenticates through the installed cell");
        assert!(sink.ids().contains(&4136), "{:?}", sink.ids());
        assert!(!sink.ids().contains(&4137), "{:?}", sink.ids());
        assert_eq!(count_occurrences(&console.bytes(), b"passphrase"), 0);
    }

    #[test]
    fn the_boot_screen_over_a_plaintext_root_closes_on_the_label_with_no_prompt() {
        // With the Supervisor wired the ESC window still opens, so the root
        // can be diagnosed; when it elapses, a root that takes no passphrase
        // leaves the bare label on that line and mounts.
        let late = LateUsersDb::new();
        let late_identity = boot_identity();
        let console = RecordingConsole::new();

        let outcome = unlock_root_disk_interactively(
            plaintext_disk(),
            &console,
            &ScriptInput::new(Vec::new()),
            &UnlockInstall {
                users: &late,
                identity: &late_identity,
                groups: &LateGroupsDb::new(),
                writable: &NoWritableRootSink,
                admin: None,
                storage_gid: &LateStorageGid::new(),
            },
            &RecordingSink::new(),
            &|| {},
            &|| {},
            Some(SupervisorWindow {
                host: &mut SilentSupervisor,
                keys: &KeysNow,
            }),
        );

        assert_eq!(outcome, UnlockOutcome::Installed);
        assert!(late.is_installed());
        let out = console.bytes();
        assert_eq!(count_occurrences(&out, SUPERVISOR_ANNOUNCE), 1);
        assert_eq!(count_occurrences(&out, FS_UNLOCKED_LINE), 1);
        assert_eq!(count_occurrences(&out, b"passphrase"), 0);
    }

    #[test]
    fn with_no_key_source_the_wait_is_erased_and_the_root_mounts_on_its_line() {
        let (late, identity, groups, gid) = (
            LateUsersDb::new(),
            boot_identity(),
            LateGroupsDb::new(),
            LateStorageGid::new(),
        );
        let console = RecordingConsole::new();

        let outcome = unlock_root_disk_interactively(
            plaintext_disk(),
            &console,
            &ScriptInput::new(Vec::new()),
            &plaintext_install(&late, &identity, &groups, &gid),
            &RecordingSink::new(),
            &|| {},
            &|| {},
            Some(SupervisorWindow {
                host: &mut SilentSupervisor,
                keys: &NoKeys,
            }),
        );

        assert_eq!(outcome, UnlockOutcome::Installed);
        assert!(late.is_installed());
        let drawn = [KEY_SOURCE_AWAITED, KEY_SOURCE_ABANDONED, FS_UNLOCKED_LINE].concat();
        assert_eq!(
            console.bytes(),
            drawn,
            "the label takes the line the wait was shown on"
        );
    }

    /// A key source that records how much of the boot screen was drawn when
    /// the window asked for it.
    struct DrawnWhenAsked<'a> {
        console: &'a RecordingConsole,
        drawn: core::cell::Cell<Option<usize>>,
    }

    impl KeyArrival for DrawnWhenAsked<'_> {
        fn key_source_ready(&self) -> bool {
            false
        }

        fn await_key_source(&self) -> bool {
            self.drawn.set(Some(self.console.bytes().len()));
            true
        }
    }

    #[test]
    fn the_window_shows_its_wait_and_announces_over_it_once_a_key_can_arrive() {
        let (late, identity, groups, gid) = (
            LateUsersDb::new(),
            boot_identity(),
            LateGroupsDb::new(),
            LateStorageGid::new(),
        );
        let console = RecordingConsole::new();
        let keys = DrawnWhenAsked {
            console: &console,
            drawn: core::cell::Cell::new(None),
        };

        let outcome = unlock_root_disk_interactively(
            plaintext_disk(),
            &console,
            &ScriptInput::new(Vec::new()),
            &plaintext_install(&late, &identity, &groups, &gid),
            &RecordingSink::new(),
            &|| {},
            &|| {},
            Some(SupervisorWindow {
                host: &mut SilentSupervisor,
                keys: &keys,
            }),
        );

        assert_eq!(outcome, UnlockOutcome::Installed);
        assert_eq!(
            keys.drawn.get(),
            Some(KEY_SOURCE_AWAITED.len()),
            "the wait is on screen while it lasts"
        );
        let out = console.bytes();
        assert!(out.starts_with(&[KEY_SOURCE_AWAITED, ANNOUNCE_OVER_WAIT].concat()));
        assert_eq!(count_occurrences(&out, SUPERVISOR_ANNOUNCE), 0);
    }

    #[test]
    fn a_key_source_ready_at_once_shows_no_wait() {
        let (late, identity, groups, gid) = (
            LateUsersDb::new(),
            boot_identity(),
            LateGroupsDb::new(),
            LateStorageGid::new(),
        );
        let console = RecordingConsole::new();

        let outcome = unlock_root_disk_interactively(
            plaintext_disk(),
            &console,
            &ScriptInput::new(Vec::new()),
            &plaintext_install(&late, &identity, &groups, &gid),
            &RecordingSink::new(),
            &|| {},
            &|| {},
            Some(SupervisorWindow {
                host: &mut SilentSupervisor,
                keys: &KeysNow,
            }),
        );

        assert_eq!(outcome, UnlockOutcome::Installed);
        let out = console.bytes();
        assert!(out.starts_with(SUPERVISOR_ANNOUNCE));
        assert_eq!(count_occurrences(&out, KEY_SOURCE_AWAITED), 0);
    }

    /// A disk that counts the reads and writes it serves.
    struct CountingBlock {
        inner: FatVecBlock,
        reads: usize,
        writes: usize,
    }

    impl CountingBlock {
        fn new(inner: FatVecBlock) -> Self {
            Self {
                inner,
                reads: 0,
                writes: 0,
            }
        }
    }

    impl Block for CountingBlock {
        fn geometry(&self) -> Result<BlockGeometry, DriverError> {
            self.inner.geometry()
        }

        fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
            self.reads += 1;
            self.inner.read_blocks(lba, buf)
        }

        fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
            self.writes += 1;
            self.inner.write_blocks(lba, buf)
        }

        fn flush(&mut self) -> Result<(), DriverError> {
            self.inner.flush()
        }
    }

    #[test]
    fn the_prefetch_reads_into_a_plaintext_root_and_writes_nothing() {
        let mut probed = CountingBlock::new(plaintext_disk());
        assert_eq!(probe_root_lock(&mut probed), Ok(RootLock::Plaintext));
        let mut warmed = CountingBlock::new(plaintext_disk());
        prefetch_root(&mut warmed);

        assert!(
            warmed.reads > probed.reads,
            "it reads past the lock into the volume"
        );
        assert_eq!(warmed.writes, 0, "nothing written before the window closes");
        let (late, identity, groups, gid) = (
            LateUsersDb::new(),
            boot_identity(),
            LateGroupsDb::new(),
            LateStorageGid::new(),
        );
        let outcome = unlock_root_disk_interactively(
            warmed.inner,
            &RecordingConsole::new(),
            &ScriptInput::new(Vec::new()),
            &plaintext_install(&late, &identity, &groups, &gid),
            &RecordingSink::new(),
            &|| {},
            &|| {},
            None,
        );
        assert_eq!(outcome, UnlockOutcome::Installed);
    }

    #[test]
    fn the_prefetch_reads_an_encrypted_root_only_as_far_as_its_lock() {
        let mut probed = CountingBlock::new(success_disk());
        assert!(matches!(
            probe_root_lock(&mut probed),
            Ok(RootLock::Passphrase(_))
        ));
        let mut warmed = CountingBlock::new(success_disk());
        prefetch_root(&mut warmed);

        assert_eq!(warmed.reads, probed.reads);
        assert_eq!(warmed.writes, 0);
    }

    /// A disk that records whether it was read before the boot screen's
    /// announcement reached `console`.
    struct ReadAfterAnnounce<'a> {
        inner: FatVecBlock,
        console: &'a RecordingConsole,
        read_early: &'a AtomicBool,
    }

    impl Block for ReadAfterAnnounce<'_> {
        fn geometry(&self) -> Result<BlockGeometry, DriverError> {
            self.inner.geometry()
        }

        fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
            if count_occurrences(&self.console.bytes(), SUPERVISOR_ANNOUNCE) == 0 {
                self.read_early.store(true, Ordering::Relaxed);
            }
            self.inner.read_blocks(lba, buf)
        }

        fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
            self.inner.write_blocks(lba, buf)
        }

        fn flush(&mut self) -> Result<(), DriverError> {
            self.inner.flush()
        }
    }

    fn plaintext_install<'a>(
        late: &'a LateUsersDb,
        identity: &'a LateIdentity,
        groups: &'a LateGroupsDb,
        storage_gid: &'a LateStorageGid,
    ) -> UnlockInstall<'a> {
        UnlockInstall {
            users: late,
            identity,
            groups,
            writable: &NoWritableRootSink,
            admin: None,
            storage_gid,
        }
    }

    #[test]
    fn the_boot_screen_opens_before_the_disk_is_read() {
        // A disk slow to answer must not hold off the Supervisor that would
        // diagnose it.
        let (late, identity, groups, gid) = (
            LateUsersDb::new(),
            boot_identity(),
            LateGroupsDb::new(),
            LateStorageGid::new(),
        );
        let console = RecordingConsole::new();
        let read_early = AtomicBool::new(false);
        let disk = ReadAfterAnnounce {
            inner: plaintext_disk(),
            console: &console,
            read_early: &read_early,
        };

        let outcome = unlock_root_disk_interactively(
            disk,
            &console,
            &ScriptInput::new(Vec::new()),
            &plaintext_install(&late, &identity, &groups, &gid),
            &RecordingSink::new(),
            &|| {},
            &|| {},
            Some(SupervisorWindow {
                host: &mut SilentSupervisor,
                keys: &KeysNow,
            }),
        );

        assert_eq!(outcome, UnlockOutcome::Installed);
        assert!(!read_early.load(Ordering::Relaxed));
    }

    #[test]
    fn a_root_that_cannot_be_mounted_says_why_and_never_draws_the_mounted_line() {
        let (late, identity, groups, gid) = (
            LateUsersDb::new(),
            boot_identity(),
            LateGroupsDb::new(),
            LateStorageGid::new(),
        );
        let console = RecordingConsole::new();
        let sink = RecordingSink::new();

        let outcome = unlock_root_disk_interactively(
            FatVecBlock::new(FAT_BOOT_SECTORS),
            &console,
            &ScriptInput::new(Vec::new()),
            &plaintext_install(&late, &identity, &groups, &gid),
            &sink,
            &|| {},
            &|| {},
            Some(SupervisorWindow {
                host: &mut SilentSupervisor,
                keys: &KeysNow,
            }),
        );

        assert_eq!(outcome, UnlockOutcome::GaveUp);
        assert!(!late.is_installed());
        let out = console.bytes();
        assert_eq!(count_occurrences(&out, FS_UNLOCKED_LINE), 0);
        let mut said = FS_GAVE_UP_PREFIX.to_vec();
        said.extend_from_slice(b"partition_table_invalid");
        assert_eq!(count_occurrences(&out, &said), 1, "{out:?}");
    }

    #[test]
    fn the_supervisor_reads_how_the_root_is_locked() {
        assert_eq!(
            supervisor_root_lock(&mut plaintext_disk()),
            Some(SupervisorRootLock::Plaintext)
        );
        assert_eq!(
            supervisor_root_lock(&mut success_disk()),
            Some(SupervisorRootLock::Passphrase)
        );
        assert_eq!(
            supervisor_root_lock(&mut FatVecBlock::new(FAT_BOOT_SECTORS)),
            None
        );
    }

    #[test]
    fn the_supervisor_mounts_a_plaintext_root_with_no_passphrase() {
        let (late, identity, groups, gid) = (
            LateUsersDb::new(),
            boot_identity(),
            LateGroupsDb::new(),
            LateStorageGid::new(),
        );
        let console = RecordingConsole::new();
        let outcome = supervisor_mount(
            &mut plaintext_disk(),
            None,
            &plaintext_install(&late, &identity, &groups, &gid),
            &RecordingSink::new(),
            &|| {},
            &mut ConsoleReport { console: &console },
        );
        assert_eq!(outcome, MountOutcome::Mounted);
        assert!(late.is_installed());
        assert_eq!(count_occurrences(&console.bytes(), b"root mounted"), 1);
    }

    #[test]
    fn the_supervisor_refuses_a_root_locked_otherwise_than_it_asked_for() {
        // The engine read the lock, asked the operator, and the disk then
        // answered differently: what was gathered no longer fits it.
        for (mut disk, passphrase) in [(plaintext_disk(), Some(PASSPHRASE)), (success_disk(), None)]
        {
            let (late, identity, groups, gid) = (
                LateUsersDb::new(),
                boot_identity(),
                LateGroupsDb::new(),
                LateStorageGid::new(),
            );
            let console = RecordingConsole::new();
            let outcome = supervisor_mount(
                &mut disk,
                passphrase,
                &plaintext_install(&late, &identity, &groups, &gid),
                &RecordingSink::new(),
                &|| {},
                &mut ConsoleReport { console: &console },
            );
            assert_eq!(outcome, MountOutcome::Failed);
            assert!(!late.is_installed());
            assert_eq!(
                count_occurrences(&console.bytes(), b"its protection changed"),
                1
            );
        }
    }

    #[test]
    fn a_wrong_supervisor_passphrase_waits_out_the_boot_delay_before_it_is_answered() {
        let (late, identity, groups, gid) = (
            LateUsersDb::new(),
            boot_identity(),
            LateGroupsDb::new(),
            LateStorageGid::new(),
        );
        let install = plaintext_install(&late, &identity, &groups, &gid);
        let console = RecordingConsole::new();
        let sink = RecordingSink::new();
        let delays = AtomicUsize::new(0);
        let mut disk = success_disk();
        let delay = || {
            assert_eq!(
                count_occurrences(&console.bytes(), b"incorrect passphrase"),
                0,
                "the pause is paid before any answer"
            );
            delays.fetch_add(1, Ordering::Relaxed);
        };

        let wrong = supervisor_mount(
            &mut disk,
            Some(b"nope"),
            &install,
            &sink,
            &delay,
            &mut ConsoleReport { console: &console },
        );
        assert_eq!(wrong, MountOutcome::WrongPassphrase);
        assert_eq!(delays.load(Ordering::Relaxed), 1);
        assert_eq!(
            count_occurrences(&console.bytes(), b"incorrect passphrase"),
            1
        );
        assert!(!late.is_installed());

        let right = supervisor_mount(
            &mut disk,
            Some(PASSPHRASE),
            &install,
            &sink,
            &|| {},
            &mut ConsoleReport { console: &console },
        );
        assert_eq!(right, MountOutcome::Mounted);
        assert!(late.is_installed());
    }

    #[test]
    fn wrong_passphrases_are_reprompted_after_a_delay_then_the_correct_one_unlocks() {
        // Two wrong passphrases are refused (each audited `4137`, no oracle),
        // each followed by the anti-brute-force delay and an
        // `Incorrect passphrase` notice, and the third, correct one unlocks
        // and installs — the same disk is reused across attempts. The prompt
        // wording, the delay count, and the notice are all asserted.
        let input = ScriptInput::new(script(&[b"nope", b"still wrong", PASSPHRASE]));
        let late = LateUsersDb::new();
        let late_identity = boot_identity();
        let sink = RecordingSink::new();
        let console = RecordingConsole::new();
        let delays = AtomicUsize::new(0);

        let outcome = unlock_root_disk_interactively(
            success_disk(),
            &console,
            &input,
            &UnlockInstall {
                users: &late,
                identity: &late_identity,
                groups: &LateGroupsDb::new(),
                writable: &NoWritableRootSink,
                admin: None,
                storage_gid: &LateStorageGid::new(),
            },
            &sink,
            &|| {
                delays.fetch_add(1, Ordering::Relaxed);
            },
            &|| {},
            None,
        );

        assert_eq!(outcome, UnlockOutcome::Installed);
        assert!(late.is_installed());
        let retries = sink.ids().iter().filter(|&&id| id == 4137).count();
        assert_eq!(retries, 2, "two wrong attempts retried: {:?}", sink.ids());
        assert!(sink.ids().contains(&4136), "{:?}", sink.ids());
        // The delay is run once per wrong attempt, before the notice appears.
        assert_eq!(delays.load(Ordering::Relaxed), 2, "one delay per wrong try");
        let out = console.bytes();
        assert_eq!(
            count_occurrences(&out, b"ARXFS passphrase: "),
            3,
            "three prompts (two wrong + the correct one) with the new wording"
        );
        assert_eq!(
            count_occurrences(&out, b"Incorrect passphrase"),
            2,
            "one notice per wrong attempt"
        );
        // The successful attempt collapses the prompt line to just the
        // filesystem label (carriage return, label, erase-to-end-of-line)
        // and then leaves a blank line before login draws its `Username:`
        // prompt.
        assert_eq!(
            count_occurrences(&out, b"\rARXFS\x1b[K"),
            1,
            "the unlock collapses the prompt line to the bare label"
        );
        assert!(
            out.ends_with(b"\r\n\r\n"),
            "a blank line separates the unlock from login"
        );
    }

    #[test]
    fn a_wrong_passphrase_is_never_given_up_on() {
        // The unlock never abandons a wrong passphrase: it keeps re-prompting
        // (each refusal delayed and audited `4137`) for as long as input
        // keeps arriving. Here the operator "gives up" — the console closes
        // after several wrong tries — so the unlock fails closed on the
        // *console* error (`4138`, `console_unreadable`), never on an
        // attempt budget. The late cell stays empty, so login is refused
        // until reboot.
        let lines = alloc::vec![b"wrong" as &[u8]; 8];
        let input = ScriptInput::new(script(&lines));
        let late = LateUsersDb::new();
        let late_identity = boot_identity();
        let sink = RecordingSink::new();
        let delays = AtomicUsize::new(0);

        let outcome = unlock_root_disk_interactively(
            success_disk(),
            &AcceptConsole,
            &input,
            &UnlockInstall {
                users: &late,
                identity: &late_identity,
                groups: &LateGroupsDb::new(),
                writable: &NoWritableRootSink,
                admin: None,
                storage_gid: &LateStorageGid::new(),
            },
            &sink,
            &|| {
                delays.fetch_add(1, Ordering::Relaxed);
            },
            &|| {},
            None,
        );

        assert_eq!(outcome, UnlockOutcome::GaveUp);
        assert!(!late.is_installed(), "no database is installed on give-up");
        // Every one of the eight wrong lines was refused and re-prompted —
        // no bounded budget cut the retries short.
        let retries = sink.ids().iter().filter(|&&id| id == 4137).count();
        assert_eq!(
            retries,
            8,
            "every wrong attempt is re-prompted: {:?}",
            sink.ids()
        );
        assert_eq!(delays.load(Ordering::Relaxed), 8, "each refusal is delayed");
        assert!(
            sink.ids().contains(&4138),
            "the fail-closed give-up is the console closing, not a budget: {:?}",
            sink.ids()
        );
    }

    #[test]
    fn a_structural_failure_gives_up_without_retrying() {
        // A disk with no partition table cannot be fixed by re-prompting,
        // so the unlock gives up on the first attempt (no `4137` retry) and
        // fails closed.
        let input = ScriptInput::new(script(&[PASSPHRASE]));
        let late = LateUsersDb::new();
        let late_identity = boot_identity();
        let sink = RecordingSink::new();

        let outcome = unlock_root_disk_interactively(
            FatVecBlock::new(64),
            &AcceptConsole,
            &input,
            &UnlockInstall {
                users: &late,
                identity: &late_identity,
                groups: &LateGroupsDb::new(),
                writable: &NoWritableRootSink,
                admin: None,
                storage_gid: &LateStorageGid::new(),
            },
            &sink,
            &|| {},
            &|| {},
            None,
        );

        assert_eq!(outcome, UnlockOutcome::GaveUp);
        assert!(!late.is_installed());
        assert!(
            !sink.ids().contains(&4137),
            "a structural failure is not retried: {:?}",
            sink.ids()
        );
        assert!(sink.ids().contains(&4138), "{:?}", sink.ids());
    }

    #[test]
    fn the_console_is_released_on_every_unlock_outcome() {
        // Regression (`plans/PI.md` P11): the unlock kthread owns console 0
        // for the passphrase prompt and must hand it back to `login` the
        // instant the unlock resolves — on **both** a successful install and
        // a fail-closed give-up. A successful unlock once skipped that
        // release, leaving the console-0 gate latched shut and the UART
        // receive interrupt masked, so neither the keyboard nor the serial
        // `login` could be typed into even though the root had mounted.
        // `unlock_root_disk_interactively` now fires `on_resolved` exactly
        // once on every internal return path; prove it for both outcomes.
        use core::cell::Cell;

        // Success path (a correct passphrase installs a database).
        let releases = Cell::new(0u32);
        let outcome = unlock_root_disk_interactively(
            success_disk(),
            &AcceptConsole,
            &ScriptInput::new(script(&[PASSPHRASE])),
            &UnlockInstall {
                users: &LateUsersDb::new(),
                identity: &boot_identity(),
                groups: &LateGroupsDb::new(),
                writable: &NoWritableRootSink,
                admin: None,
                storage_gid: &LateStorageGid::new(),
            },
            &RecordingSink::new(),
            &|| {},
            &|| releases.set(releases.get() + 1),
            None,
        );
        assert_eq!(outcome, UnlockOutcome::Installed);
        assert_eq!(
            releases.get(),
            1,
            "a successful unlock releases console 0 exactly once"
        );

        // Give-up path (a structural failure resolves with no database).
        let releases = Cell::new(0u32);
        let outcome = unlock_root_disk_interactively(
            FatVecBlock::new(64),
            &AcceptConsole,
            &ScriptInput::new(script(&[PASSPHRASE])),
            &UnlockInstall {
                users: &LateUsersDb::new(),
                identity: &boot_identity(),
                groups: &LateGroupsDb::new(),
                writable: &NoWritableRootSink,
                admin: None,
                storage_gid: &LateStorageGid::new(),
            },
            &RecordingSink::new(),
            &|| {},
            &|| releases.set(releases.get() + 1),
            None,
        );
        assert_eq!(outcome, UnlockOutcome::GaveUp);
        assert_eq!(
            releases.get(),
            1,
            "a fail-closed unlock still releases console 0 exactly once"
        );
    }

    #[test]
    fn an_unreadable_console_gives_up_fail_closed() {
        // a console whose read faults cannot deliver a passphrase;
        // the unlock fails closed rather than mounting under an empty or
        // fabricated secret, and never touches the disk.
        let input = ScriptInput::failing();
        let late = LateUsersDb::new();
        let late_identity = boot_identity();
        let sink = RecordingSink::new();

        let outcome = unlock_root_disk_interactively(
            success_disk(),
            &AcceptConsole,
            &input,
            &UnlockInstall {
                users: &late,
                identity: &late_identity,
                groups: &LateGroupsDb::new(),
                writable: &NoWritableRootSink,
                admin: None,
                storage_gid: &LateStorageGid::new(),
            },
            &sink,
            &|| {},
            &|| {},
            None,
        );

        assert_eq!(outcome, UnlockOutcome::GaveUp);
        assert!(!late.is_installed());
        assert!(
            !sink.ids().contains(&4133),
            "the disk is never unlocked: {:?}",
            sink.ids()
        );
        assert!(sink.ids().contains(&4138), "{:?}", sink.ids());
    }

    #[test]
    fn an_over_long_line_is_a_wrong_attempt_not_a_truncated_secret() {
        // a line longer than MAX_ROOT_PASSPHRASE_LEN is drained and
        // counted as a wrong attempt (audited `4137`), never silently
        // truncated to a shorter secret; the next, correct line unlocks.
        let over_long = alloc::vec![b'a'; MAX_ROOT_PASSPHRASE_LEN + 16];
        let input = ScriptInput::new(script(&[&over_long, PASSPHRASE]));
        let late = LateUsersDb::new();
        let late_identity = boot_identity();
        let sink = RecordingSink::new();

        let outcome = unlock_root_disk_interactively(
            success_disk(),
            &AcceptConsole,
            &input,
            &UnlockInstall {
                users: &late,
                identity: &late_identity,
                groups: &LateGroupsDb::new(),
                writable: &NoWritableRootSink,
                admin: None,
                storage_gid: &LateStorageGid::new(),
            },
            &sink,
            &|| {},
            &|| {},
            None,
        );

        assert_eq!(outcome, UnlockOutcome::Installed);
        assert!(late.is_installed());
        let retries = sink.ids().iter().filter(|&&id| id == 4137).count();
        assert_eq!(
            retries,
            1,
            "the over-long line is one retry: {:?}",
            sink.ids()
        );
    }

    // --- with_system_volume (the one design-B /System mount seam) --------

    #[test]
    fn with_system_volume_runs_the_continuation_and_returns_its_result() {
        // The one mount seam: it mounts the read-only
        // `/System` volume, runs `f` against the still-open volume, and
        // returns `Some(f(..))`. The volume reads cleanly inside `f` — proven
        // by reading the `Drivers` store directory back out.
        let mut disk = success_disk();
        let sink = RecordingSink::new();

        let found_store = with_system_volume(&mut disk, &sink, None, |volume| {
            let root = volume.root();
            volume.lookup(root, b"Drivers").is_ok()
        });

        assert_eq!(
            found_store,
            Some(true),
            "the continuation runs against the mounted volume and its result is returned"
        );
        assert!(sink.ids().contains(&4140), "{:?}", sink.ids());
        assert!(!sink.ids().contains(&4141), "{:?}", sink.ids());
    }

    #[test]
    fn with_system_volume_returns_none_and_never_runs_the_continuation_without_a_volume() {
        // with no recognised partition table there is no
        // `/System` volume, so `with_system_volume` returns `None` without
        // ever running the continuation, audits the unavailable case
        // (`4141`), and never the mounted one (`4140`).
        let mut disk = FatVecBlock::new(64);
        let sink = RecordingSink::new();

        let ran = with_system_volume(&mut disk, &sink, None, |_volume| {
            panic!("the continuation must not run when no /System volume mounts");
        });

        assert_eq!(ran, None);
        assert!(sink.ids().contains(&4141), "{:?}", sink.ids());
        assert!(!sink.ids().contains(&4140), "{:?}", sink.ids());
    }

    /// A [`SupervisorHost`] that renders nothing and performs nothing.
    ///
    /// The boot-session tests drive only the REPL's `continue` command, so
    /// every data and control method is a no-op; a real host is exercised by
    /// the `lib/supervisor` and `supervisor_host` tests.
    struct SilentSupervisor;

    impl SupervisorHost for SilentSupervisor {
        fn version(&mut self, _out: &mut dyn SupReport) {}
        fn memory(&mut self, _out: &mut dyn SupReport) {}
        fn memory_map(&mut self, _out: &mut dyn SupReport) {}
        fn cpu(&mut self, _out: &mut dyn SupReport) {}
        fn hardware(&mut self, _out: &mut dyn SupReport) {}
        fn disks(&mut self, _out: &mut dyn SupReport) {}
        fn partitions(&mut self, _device: &str, _out: &mut dyn SupReport) {}
        fn arxfs_status(&mut self, _out: &mut dyn SupReport) {}
        fn list(&mut self, _path: Option<&str>, _out: &mut dyn SupReport) {}
        fn log_tail(&mut self, _count: Option<usize>, _out: &mut dyn SupReport) {}
        fn panic_log(&mut self, _out: &mut dyn SupReport) {}
        fn uptime(&mut self, _out: &mut dyn SupReport) {}
        fn date(&mut self, _out: &mut dyn SupReport) {}
        fn scan_disk(
            &mut self,
            _device: &str,
            _out: &mut dyn SupReport,
            _abort: &mut dyn FnMut() -> bool,
        ) -> TestOutcome {
            TestOutcome::Passed
        }
        fn root_lock(&mut self) -> Option<tairix_supervisor::RootLock> {
            None
        }
        fn mount(&mut self, _passphrase: Option<&[u8]>, _out: &mut dyn SupReport) -> MountOutcome {
            MountOutcome::Failed
        }
        fn reboot(&mut self) {}
        fn poweroff(&mut self) {}
        fn takeover_memtest(&mut self, _out: &mut dyn SupReport) {}
        fn audit(&mut self, _event: SupervisorEvent) {}
    }

    /// Console 0 that can deliver a key at once, as a UART can.
    struct KeysNow;

    impl KeyArrival for KeysNow {
        fn key_source_ready(&self) -> bool {
            true
        }

        fn await_key_source(&self) -> bool {
            true
        }
    }

    /// Console 0 whose input device never binds.
    struct NoKeys;

    impl KeyArrival for NoKeys {
        fn key_source_ready(&self) -> bool {
            false
        }

        fn await_key_source(&self) -> bool {
            false
        }
    }

    /// One REPL command line, terminated as the console would deliver it.
    fn repl_script(line: &[u8]) -> Vec<u8> {
        let mut bytes = line.to_vec();
        bytes.push(b'\n');
        bytes
    }

    /// Console bytes that enter the Supervisor from the boot screen and then
    /// type `line`.
    ///
    /// The first `ESC` opens the window; the second is what the
    /// lone-`ESC`-versus-CSI re-poll consumes (the scripted reader answers it
    /// immediately, so the window resolves without waiting), leaving the line
    /// for the REPL to read.
    fn boot_screen_script(line: &[u8]) -> Vec<u8> {
        let mut bytes = alloc::vec![0x1b, 0x1b];
        bytes.extend_from_slice(&repl_script(line));
        bytes
    }

    #[test]
    fn a_supervisor_continue_gui_records_the_graphical_session() {
        // The operator's `continue gui` must reach the cell `boot_session_get`
        // reports from, so `login` starts the desktop for this one boot.
        let cell = LateBootSession::new();
        let input = ScriptInput::new(boot_screen_script(b"continue gui"));

        let window = esc_boot_window(
            &AcceptConsole,
            &input,
            &mut SupervisorWindow {
                host: &mut SilentSupervisor,
                keys: &KeysNow,
            },
            &cell,
        );

        assert!(matches!(window, EscWindow::Resumed));
        assert_eq!(cell.get(), BootSession::Graphical);
    }

    #[test]
    fn a_bare_supervisor_continue_records_no_session() {
        // No operand is no choice: the cell stays unset so the stored
        // `os.loginType` default decides, exactly as a boot that never
        // entered the Supervisor.
        let cell = LateBootSession::new();
        let input = ScriptInput::new(boot_screen_script(b"continue"));

        esc_boot_window(
            &AcceptConsole,
            &input,
            &mut SupervisorWindow {
                host: &mut SilentSupervisor,
                keys: &KeysNow,
            },
            &cell,
        );

        assert_eq!(cell.get(), BootSession::Unset);
    }

    #[test]
    fn a_second_supervisor_entry_does_not_overwrite_the_first_choice() {
        // Set-once: the lone-`ESC`-at-the-passphrase-prompt entry records a
        // choice through the same funnel, and a later entry cannot rewrite
        // one already made.
        let cell = LateBootSession::new();
        let first = ScriptInput::new(repl_script(b"continue text"));
        enter_supervisor(&AcceptConsole, &first, &mut SilentSupervisor, &cell);
        assert_eq!(cell.get(), BootSession::Text);

        let second = ScriptInput::new(repl_script(b"continue gui"));
        enter_supervisor(&AcceptConsole, &second, &mut SilentSupervisor, &cell);
        assert_eq!(cell.get(), BootSession::Text);
    }
}
