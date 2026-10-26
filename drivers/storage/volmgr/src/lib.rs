//! TAIRiX **volume-manager policy driver** — shared library.
//!
//! This crate is a `lib` (the loadable-module identity — the [`BIND_KEYS`]
//! bind table `devmgr` matches a discovered block-service storage node
//! against — plus the host-testable policy engine: the partition +
//! filesystem probe plan ([`plan`]) and the catalog naming policy
//! ([`name`])) **and** a `Run` binary (`src/main.rs`, the autoloaded
//! per-node policy process). The blkio block client it probes a device
//! through is the shared `tairix_blkclient::RemoteBlock`, opened
//! read-only: this driver only ever inspects a device's layout and commits
//! nothing to it.
//!
//! # What it is
//!
//! The volume manager owns volume *policy* the way `devmgr` owns driver
//! policy (`plans/DEVICES.md` D3c). A block driver that brings a
//! hot-pluggable unit up publishes it as a storage-class hardware-tree
//! node carrying two transport grants — a blkio call endpoint and a shared
//! data window (`tairix_abi::blkio`). `devmgr` matches that node against
//! this crate's bind table and loads one volume-manager instance for it;
//! the kernel spawns the instance holding **exactly that node's** grants,
//! so an instance can probe and publish its own unit and can never reach a
//! sibling device's transport (no ambient authority). The instance:
//!
//! 1. connects the blkio client and validates the device geometry,
//! 2. probes the partition table (`lib/partition`; a device with no table
//!    is probed whole),
//! 3. probes each candidate extent's head for a supported filesystem
//!    signature (`lib/fsprobe`),
//! 4. derives the catalog name from the volume alone (its own label in
//!    kebab case, else `<transport>_<fingerprint>`; a label in use or
//!    reserved takes `_<fingerprint>` of the volume's identity — [`name`]),
//!    and
//! 5. asks the kernel to attach each recognised volume (the
//!    `CAP_FS_MOUNT`-gated, audited `volume_attach` syscall — the kernel
//!    re-validates everything and publishes the volume node its format's
//!    user-space driver serves it through).
//!
//! It then exits `0`: publication is a run-to-completion job, and the
//! kernel-held volume outlives the instance. Removal (detach, retained dirty
//! state) is the kernel's.
//!
//! Nothing here names a bus, a controller, or a vendor: the bind table
//! selects on the block-service node's own compatible key, and every byte
//! of device data arrives over the public blkio ABI.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(test)]
extern crate alloc;

pub mod name;
pub mod plan;

use tairix_abi::raid_ipc::RAID_ARRAY_COMPATIBLE;
use tairix_abi::{compatible_key, CapabilityId, DriverBindKey};

/// The bind priority [`BIND_KEYS`] carries. An exact `compatible`-string
/// match on the block-service node ranks like the other exact-compatible
/// drivers.
const BIND_PRIORITY: u16 = 10;

/// The capabilities the driver runs with, which its signed manifest requests:
/// the data window and block endpoint its block driver forwarded, the kernel
/// attach of each volume, publishing an array member's node, and its
/// diagnostics. No MMIO, DMA or interrupt authority.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::SHM,
    CapabilityId::IPC_ENDPOINT,
    CapabilityId::FS_MOUNT,
    CapabilityId::HW_EMIT,
    CapabilityId::LOG_EMIT,
];

/// This driver's hardware bind table: every block-service storage node
/// whose volumes this policy driver owns — the per-LUN node the USB
/// mass-storage class driver emits, and the composed-array node the RAID
/// array composer emits. Future hot-pluggable block sources join by adding
/// their node's compatible key here — never by naming a bus in the engine.
/// This `const` is the single source of truth the signed-manifest bind
/// table is authored from (`tools/xtask` image builder) and `devmgr`
/// resolves a discovered storage node against.
///
/// An array is deliberately probed by the *same* driver as a leaf disk and
/// through the same code: to this policy engine a composed array is simply a
/// block device with geometry, so its partitions and filesystems are found,
/// named, and attached by one implementation rather than a second one that
/// could drift from it. That an array is several disks underneath is the
/// composer's business, and arrays stack over arrays for free because the
/// node an array publishes is indistinguishable in kind from a disk's.
pub const BIND_KEYS: &[DriverBindKey] = &[
    DriverBindKey::new(BIND_PRIORITY, compatible_key!(b"tairix,usb-msd-lun")),
    DriverBindKey::new(BIND_PRIORITY, compatible_key!(RAID_ARRAY_COMPATIBLE)),
];
