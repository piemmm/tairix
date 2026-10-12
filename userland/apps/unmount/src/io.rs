//! The seams through which `unmount` touches the outside world.
//!
//! The mount table arrives through the shared
//! [`Transport`](tairix_procinfo::Transport) seam (the `sysinfo-v1`
//! `MOUNT_LIST` paging walk `mount` and `df` use too); only the
//! privileged detach and the output streams need seams of their own.
//! Keeping them behind object-safe traits lets the engine in
//! [`crate::client`] run against in-memory fixtures with no kernel,
//! mirroring the seam discipline of the other userland tools (`mount`'s
//! `Mounter`, `df`'s `PathProbe`).

use tairix_abi::volume::VOLUME_ID_LEN;
use tairix_abi::Errno;

/// Performs the privileged volume detach.
///
/// The implementation performs no authorisation of its own: the kernel's
/// `volume_detach` path requires `CAP_FS_MOUNT`, re-validates the
/// identity against the attached volumes, and audits every decision. A
/// refusal surfaces as the exact [`Errno`] the kernel chose.
pub trait Detacher {
    /// Detach the volume published under `volume_id`; `force` selects
    /// the audited force-unmount that discards retained uncommitted
    /// data when a clean commit is impossible.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the kernel raises — e.g. [`Errno::NotFound`] for an
    /// identity no attached volume carries, [`Errno::DeviceFault`] for a
    /// plain detach of an unavailable volume, or
    /// [`Errno::PermissionDenied`] when the caller lacks `CAP_FS_MOUNT`.
    fn detach(&self, volume_id: [u8; VOLUME_ID_LEN], force: bool) -> Result<(), Errno>;
}

pub use tairix_util::output::Output;
