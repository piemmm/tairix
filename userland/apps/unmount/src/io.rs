//! The seams through which `unmount` touches the outside world.
//!
//! The mount and view listings arrive through the shared
//! [`Transport`](tairix_procinfo::Transport) seam (the `sysinfo-v1`
//! `MOUNT_LIST` and `VIEW_LIST` walks `mount` and `df` read too); only the
//! detach, the path a name spells, and the output streams need seams of
//! their own.
//! Keeping them behind object-safe traits lets the engine in
//! [`crate::client`] run against in-memory fixtures with no kernel,
//! mirroring the seam discipline of the other userland tools (`mount`'s
//! `Mounter`, `df`'s `PathProbe`).

use alloc::string::String;

use tairix_abi::volume::VOLUME_ID_LEN;
use tairix_abi::Errno;

/// Performs the volume detach, and canonicalises the path naming one.
///
/// The implementation performs no authorisation of its own: the kernel's
/// `volume_detach` path lets the user who attached a disk image or a network
/// share detach it, and requires `CAP_FS_MOUNT` for every other volume,
/// re-validates the identity against the attached volumes, and audits every
/// decision. A refusal surfaces as the exact [`Errno`] the kernel chose.
pub trait Detacher {
    /// The user a detach is made as, as the kernel attests them, or `None`
    /// when it will not say.
    fn user(&self) -> Option<u32>;

    /// Detach the volume published under `volume_id`; `force` selects
    /// the audited force-unmount that discards retained uncommitted
    /// data when a clean commit is impossible.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the kernel raises, as
    /// [`VolumeDetachRequest`](tairix_abi::volume::VolumeDetachRequest) lists
    /// them — e.g. [`Errno::NotFound`] for an identity no attached volume
    /// carries, [`Errno::Busy`] while a descriptor, a working directory or
    /// another operation holds it, [`Errno::DeviceFault`] or
    /// [`Errno::NotEmpty`] for a plain detach a vanished or conflicted
    /// volume's writes refuse, or [`Errno::PermissionDenied`] for a volume
    /// the caller may not detach.
    fn detach(&self, volume_id: [u8; VOLUME_ID_LEN], force: bool) -> Result<(), Errno>;

    /// The directory `path` names, absolute and canonical, as util-linux
    /// canonicalises a mount point.
    ///
    /// # Errors
    ///
    /// The refusal to resolve it, [`Errno::NotFound`] for a directory that
    /// does not exist.
    fn canonical(&self, path: &str) -> Result<String, Errno>;
}

pub use tairix_util::output::Output;
