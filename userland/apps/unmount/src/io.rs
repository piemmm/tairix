//! The seams through which `unmount` touches the outside world.
//!
//! The mount and view listings arrive through the shared
//! [`Transport`](tairix_procinfo::Transport) seam (the `sysinfo-v1`
//! `MOUNT_LIST` and `VIEW_LIST` walks `mount` and `df` read too), and who
//! names a volume and how the path they spell resolves through the shared
//! [`Caller`] seam the detach extends; only the detach and the output streams
//! need seams of their own.
//! Keeping them behind object-safe traits lets the engine in
//! [`crate::client`] run against in-memory fixtures with no kernel,
//! mirroring the seam discipline of the other userland tools (`mount`'s
//! `Mounter`, `df`'s `PathProbe`).

use tairix_abi::volume::VOLUME_ID_LEN;
use tairix_abi::Errno;
use tairix_procinfo::Caller;

/// Performs the volume detach, for the [`Caller`] that names it.
///
/// The implementation performs no authorisation of its own: the kernel's
/// `volume_detach` path lets the user who attached a disk image or a network
/// share detach it, and requires `CAP_FS_MOUNT` for every other volume,
/// re-validates the identity against the attached volumes, and audits every
/// decision. A refusal surfaces as the exact [`Errno`] the kernel chose.
pub trait Detacher: Caller {
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
}

pub use tairix_util::output::Output;
